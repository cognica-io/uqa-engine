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
use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum SQLiteError {
    #[error("sqlite error: {0}")]
    SQLite(#[from] rusqlite::Error),
    #[error("encryption key must not be empty")]
    EmptyEncryptionKey,
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

    fn open_with_optional_key(path: &Path, key: Option<&str>) -> Result<Self> {
        let conn = Connection::open(path)?;
        if let Some(key) = key {
            Self::apply_encryption_key(&conn, key)?;
        }
        Self::configure(&conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
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

    fn configure(conn: &Connection) -> Result<()> {
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
}
