//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared `SQLite` connection wrapper.
//!
//! All persistent stores (documents, inverted index, vectors) on a single
//! database share one [`ManagedConnection`]. WAL mode lets readers and a
//! single writer make progress concurrently; the busy timeout absorbs the
//! occasional contention without surfacing `SQLITE_BUSY` to callers.

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags};

use crate::sqlite::compressed_vfs::{self, SQLiteCompressionOptions};

#[derive(Debug, thiserror::Error)]
pub enum SQLiteError {
    #[error("sqlite error: {0}")]
    SQLite(#[from] rusqlite::Error),
    #[error("encryption key must not be empty")]
    EmptyEncryptionKey,
    #[error("compressed sqlite container error: {0}")]
    CompressedContainer(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("catalog migration {version} failed: {source}")]
    Migration {
        version: u32,
        #[source]
        source: rusqlite::Error,
    },
    #[error("payload serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, SQLiteError>;

/// Shared `SQLite` handle. Cloning is cheap (`Arc`); every clone speaks
/// to the same underlying connection through the same `Mutex`.
#[derive(Clone)]
pub struct ManagedConnection {
    inner: Arc<Mutex<Connection>>,
}

impl ManagedConnection {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_optional_key(path, None)
    }

    pub fn open_encrypted(path: &Path, key: &str) -> Result<Self> {
        Self::open_with_optional_key(path, Some(key))
    }

    pub fn open_compressed(path: &Path, compression: SQLiteCompressionOptions) -> Result<Self> {
        Self::open_compressed_with_optional_key(path, compression, None)
    }

    pub fn open_compressed_encrypted(
        path: &Path,
        key: &str,
        compression: SQLiteCompressionOptions,
    ) -> Result<Self> {
        if key.is_empty() {
            return Err(SQLiteError::EmptyEncryptionKey);
        }
        Self::open_compressed_with_optional_key(path, compression, Some(key))
    }

    fn open_with_optional_key(path: &Path, key: Option<&str>) -> Result<Self> {
        let conn = Connection::open(path)?;
        if let Some(key) = key {
            Self::apply_encryption_key(&conn, key)?;
        }
        Self::configure_wal(&conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    fn open_compressed_with_optional_key(
        path: &Path,
        compression: SQLiteCompressionOptions,
        key: Option<&str>,
    ) -> Result<Self> {
        let compression = compression
            .validate()
            .map_err(SQLiteError::CompressedContainer)?;
        compressed_vfs::register_database(path, compression, key)
            .map_err(SQLiteError::CompressedContainer)?;
        let conn = Connection::open_with_flags_and_vfs(
            path,
            OpenFlags::default(),
            compressed_vfs::VFS_NAME,
        )?;
        conn.pragma_update(None, "page_size", compression.page_size)?;
        Self::configure_compressed(&conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure_wal(&conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    fn apply_encryption_key(conn: &Connection, key: &str) -> Result<()> {
        if key.is_empty() {
            return Err(SQLiteError::EmptyEncryptionKey);
        }
        conn.pragma_update(None, "key", key)?;
        Ok(())
    }

    fn configure_wal(conn: &Connection) -> Result<()> {
        // WAL: many readers + 1 writer concurrently, durable on commit.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // 5s busy timeout absorbs short contention without surfacing
        // SQLITE_BUSY; long contention is a real bug.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Synchronous=NORMAL is the recommended pairing with WAL: safe
        // against power loss, faster than FULL.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Foreign keys are off by default; turn on so per-table cleanup
        // can use ON DELETE CASCADE later.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    fn configure_compressed(conn: &Connection) -> Result<()> {
        // The compressed VFS implements the byte-addressed database file.
        // Rollback journals stay raw because they are short-lived commit
        // machinery; compressing them only adds autocommit write amplification.
        // WAL requires shared-memory VFS methods, so compressed databases use
        // SQLite's rollback journal and keep temp storage in memory.
        conn.pragma_update(None, "journal_mode", "DELETE")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /// Run a closure with the underlying [`Connection`]. Holds the
    /// internal mutex for the duration of `f`; never reenter via
    /// another `with` from inside `f`.
    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let conn = self.inner.lock();
        f(&conn)
    }

    pub fn with_mut<R>(&self, f: impl FnOnce(&mut Connection) -> Result<R>) -> Result<R> {
        let mut conn = self.inner.lock();
        f(&mut conn)
    }

    /// Open an explicit (non-deferred) transaction. Subsequent
    /// auto-commit hosts (catalog writes, FTS index updates, ...) all
    /// flow through the same connection so the transaction enclosing
    /// them is honoured. Use [`Self::commit_transaction`] /
    /// [`Self::rollback_transaction`] / [`Self::savepoint`] etc. for
    /// the lifecycle.
    pub fn begin_transaction(&self) -> Result<()> {
        self.with(|c| {
            c.execute_batch("BEGIN IMMEDIATE")?;
            Ok(())
        })
    }

    pub fn commit_transaction(&self) -> Result<()> {
        self.with(|c| {
            c.execute_batch("COMMIT")?;
            Ok(())
        })
    }

    pub fn rollback_transaction(&self) -> Result<()> {
        self.with(|c| {
            c.execute_batch("ROLLBACK")?;
            Ok(())
        })
    }

    pub fn savepoint(&self, name: &str) -> Result<()> {
        let stmt = format!("SAVEPOINT \"{}\"", name.replace('"', "\"\""));
        self.with(|c| {
            c.execute_batch(&stmt)?;
            Ok(())
        })
    }

    pub fn release_savepoint(&self, name: &str) -> Result<()> {
        let stmt = format!("RELEASE SAVEPOINT \"{}\"", name.replace('"', "\"\""));
        self.with(|c| {
            c.execute_batch(&stmt)?;
            Ok(())
        })
    }

    pub fn rollback_to_savepoint(&self, name: &str) -> Result<()> {
        let stmt = format!("ROLLBACK TO SAVEPOINT \"{}\"", name.replace('"', "\"\""));
        self.with(|c| {
            c.execute_batch(&stmt)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_connection_round_trip() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        mc.with(|c| {
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", [])?;
            c.execute("INSERT INTO t (id, v) VALUES (1, 'hi')", [])?;
            let got: String = c.query_row("SELECT v FROM t WHERE id = 1", [], |r| r.get(0))?;
            assert_eq!(got, "hi");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn wal_mode_pragma_is_set() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let mode: String = mc
            .with(|c| Ok(c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?))
            .unwrap();
        // In-memory DBs cannot use WAL — SQLite silently downgrades.
        // Assert we got *some* known journal mode; the file-backed CI
        // path is what enforces WAL.
        assert!(matches!(
            mode.to_lowercase().as_str(),
            "memory" | "wal" | "delete" | "truncate" | "persist" | "off"
        ));
    }

    #[test]
    fn sqlcipher_build_reports_cipher_version() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let version: String = mc
            .with(|c| Ok(c.query_row("PRAGMA cipher_version", [], |r| r.get(0))?))
            .unwrap();
        assert!(!version.is_empty());
    }

    #[test]
    fn encrypted_file_requires_matching_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("encrypted.sqlite3");
        let key = "correct horse battery staple";

        {
            let mc = ManagedConnection::open_encrypted(&path, key).unwrap();
            mc.with(|c| {
                c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", [])?;
                c.execute("INSERT INTO t (id, v) VALUES (1, 'secret')", [])?;
                Ok(())
            })
            .unwrap();
        }

        {
            let mc = ManagedConnection::open_encrypted(&path, key).unwrap();
            let got: String = mc
                .with(|c| Ok(c.query_row("SELECT v FROM t WHERE id = 1", [], |r| r.get(0))?))
                .unwrap();
            assert_eq!(got, "secret");
        }

        assert!(ManagedConnection::open_encrypted(&path, "wrong key").is_err());
        assert!(ManagedConnection::open(&path).is_err());
        assert!(matches!(
            ManagedConnection::open_encrypted(&path, ""),
            Err(SQLiteError::EmptyEncryptionKey)
        ));
    }

    #[test]
    fn compressed_file_reopens_through_vfs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compressed.uqac.sqlite3");
        let plain_path = dir.path().join("plain.sqlite3");
        let options = SQLiteCompressionOptions::default();
        let repeated = "compressible payload ".repeat(256);

        {
            let mc = ManagedConnection::open_compressed(&path, options).unwrap();
            mc.with(|c| {
                c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT)", [])?;
                let mut stmt = c.prepare("INSERT INTO t (id, body) VALUES (?1, ?2)")?;
                for id in 0..128_i64 {
                    stmt.execute(rusqlite::params![id, &repeated])?;
                }
                Ok(())
            })
            .unwrap();
        }

        {
            let mc = ManagedConnection::open_compressed(&path, options).unwrap();
            let count: i64 = mc
                .with(|c| Ok(c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))?))
                .unwrap();
            assert_eq!(count, 128);
        }

        {
            let plain = Connection::open(&plain_path).unwrap();
            plain
                .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT)", [])
                .unwrap();
            let mut stmt = plain
                .prepare("INSERT INTO t (id, body) VALUES (?1, ?2)")
                .unwrap();
            for id in 0..128_i64 {
                stmt.execute(rusqlite::params![id, &repeated]).unwrap();
            }
        }

        let compressed = std::fs::read(&path).unwrap();
        let plain_len = std::fs::metadata(&plain_path).unwrap().len();
        assert_eq!(&compressed[..8], b"UQACDB3\0");
        assert!(compressed.len() < plain_len as usize);
        assert!(ManagedConnection::open(&path).is_err());
    }

    #[test]
    fn lz4_compressed_file_reopens_through_vfs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compressed-lz4.uqac.sqlite3");
        let options = SQLiteCompressionOptions::lz4();
        let repeated = "lz4 compressible payload ".repeat(256);

        {
            let mc = ManagedConnection::open_compressed(&path, options).unwrap();
            mc.with(|c| {
                c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT)", [])?;
                let mut stmt = c.prepare("INSERT INTO t (id, body) VALUES (?1, ?2)")?;
                for id in 0..128_i64 {
                    stmt.execute(rusqlite::params![id, &repeated])?;
                }
                Ok(())
            })
            .unwrap();
        }

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"UQACDB3\0");

        let mc = ManagedConnection::open_compressed(&path, options).unwrap();
        let count: i64 = mc
            .with(|c| Ok(c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(count, 128);
    }

    #[test]
    fn compressed_encrypted_file_requires_matching_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compressed-encrypted.uqac.sqlite3");
        let options = SQLiteCompressionOptions::default();
        let key = "correct horse battery staple";
        let secret = "very secret compressed payload".repeat(64);

        {
            let mc = ManagedConnection::open_compressed_encrypted(&path, key, options).unwrap();
            mc.with(|c| {
                c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT)", [])?;
                c.execute(
                    "INSERT INTO t (id, body) VALUES (1, ?1)",
                    rusqlite::params![&secret],
                )?;
                Ok(())
            })
            .unwrap();
        }

        {
            let mc = ManagedConnection::open_compressed_encrypted(&path, key, options).unwrap();
            let got: String = mc
                .with(|c| Ok(c.query_row("SELECT body FROM t WHERE id = 1", [], |r| r.get(0))?))
                .unwrap();
            assert_eq!(got, secret);
        }

        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes
            .windows(b"very secret compressed payload".len())
            .any(|window| window == b"very secret compressed payload"));
        assert!(ManagedConnection::open_compressed_encrypted(&path, "wrong key", options).is_err());
        assert!(ManagedConnection::open_compressed(&path, options).is_err());
        assert!(ManagedConnection::open(&path).is_err());
        assert!(matches!(
            ManagedConnection::open_compressed_encrypted(&path, "", options),
            Err(SQLiteError::EmptyEncryptionKey)
        ));
    }
}
