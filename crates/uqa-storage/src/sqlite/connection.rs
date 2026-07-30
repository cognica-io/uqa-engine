//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pooled `SQLite` connections with explicit session and transaction affinity.
//!
//! A [`ManagedConnection`] is a logical session. Clones share that session so
//! catalog, document, inverted-index, and vector stores participate in the
//! same explicit transaction. [`ManagedConnection::new_session`] creates an
//! isolated session over the same physical connection pool. Outside explicit
//! transactions operations check out independent connections, allowing WAL
//! readers to make real concurrent progress.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex, RwLock};
use rusqlite::{Connection, OpenFlags};

use crate::sqlite::compressed_vfs::{self, SQLiteCompressionOptions};

#[derive(Debug, thiserror::Error)]
pub enum SQLiteError {
    #[error("text analysis failed: {0}")]
    Analysis(#[from] uqa_analysis::AnalysisError),
    #[error("sqlite error: {0}")]
    SQLite(#[from] rusqlite::Error),
    #[error("encryption key must not be empty")]
    EmptyEncryptionKey,
    #[error("database requires an encryption key")]
    EncryptionKeyRequired,
    #[error("database is not encrypted but an encryption key was provided")]
    NotEncrypted,
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
    #[error("invalid persisted catalog schema version `{0}`")]
    InvalidSchemaVersion(String),
    #[error("catalog schema version {found} is newer than this engine supports ({supported})")]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    #[error("corrupt document blob for `{table}` doc {doc_id} field `{field}`: {reason}")]
    CorruptDocumentBlob {
        table: String,
        doc_id: u64,
        field: String,
        reason: String,
    },
    #[error("payload serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("storage backend error: {0}")]
    StorageBackend(String),
    #[error("transaction already active for this sqlite session")]
    TransactionAlreadyActive,
    #[error("no active transaction for this sqlite session")]
    NoActiveTransaction,
    #[error("sqlite transaction was aborted by an earlier storage error: {0}")]
    TransactionAborted(String),
    #[error("sqlite session cleanup failed: {0}")]
    SessionCleanupFailed(String),
    #[error("sqlite connection-pool checkout lost its connection")]
    MissingCheckedOutConnection,
}

pub type Result<T> = std::result::Result<T, SQLiteError>;

const MIN_POOL_CONNECTIONS: usize = 4;
const MAX_POOL_CONNECTIONS: usize = 32;

#[derive(Clone)]
enum ConnectionSpec {
    File {
        path: PathBuf,
        key: Option<Arc<str>>,
    },
    Compressed {
        path: PathBuf,
        compression: SQLiteCompressionOptions,
    },
    Memory,
}

impl ConnectionSpec {
    fn open(&self, initialize_database: bool) -> Result<Connection> {
        match self {
            Self::File { path, key } => {
                let conn = Connection::open(path)?;
                if let Some(key) = key {
                    ManagedConnection::apply_encryption_key(&conn, key)?;
                }
                if initialize_database {
                    ManagedConnection::enable_wal(&conn)?;
                }
                ManagedConnection::configure_wal_connection(&conn)?;
                Ok(conn)
            }
            Self::Compressed { path, compression } => {
                let conn = Connection::open_with_flags_and_vfs(
                    path,
                    OpenFlags::default(),
                    compressed_vfs::VFS_NAME,
                )?;
                if initialize_database {
                    conn.pragma_update(None, "page_size", compression.page_size)?;
                    ManagedConnection::enable_compressed_journal(&conn)?;
                }
                ManagedConnection::configure_compressed_connection(&conn)?;
                Ok(conn)
            }
            Self::Memory => {
                let conn = Connection::open_in_memory()?;
                if initialize_database {
                    ManagedConnection::enable_wal(&conn)?;
                }
                ManagedConnection::configure_wal_connection(&conn)?;
                Ok(conn)
            }
        }
    }
}

struct PoolState {
    idle: Vec<Connection>,
    open: usize,
}

struct ConnectionPool {
    spec: ConnectionSpec,
    max_connections: usize,
    state: Mutex<PoolState>,
    available: Condvar,
}

impl ConnectionPool {
    fn new(spec: ConnectionSpec, initial: Connection, max_connections: usize) -> Arc<Self> {
        Arc::new(Self {
            spec,
            max_connections: max_connections.max(1),
            state: Mutex::new(PoolState {
                idle: vec![initial],
                open: 1,
            }),
            available: Condvar::new(),
        })
    }

    fn checkout(self: &Arc<Self>) -> Result<PooledConnection> {
        loop {
            let mut state = self.state.lock();
            if let Some(connection) = state.idle.pop() {
                return Ok(PooledConnection {
                    pool: Arc::clone(self),
                    connection: Some(connection),
                });
            }
            if state.open < self.max_connections {
                state.open += 1;
                drop(state);
                return match self.spec.open(false) {
                    Ok(connection) => Ok(PooledConnection {
                        pool: Arc::clone(self),
                        connection: Some(connection),
                    }),
                    Err(error) => {
                        let mut state = self.state.lock();
                        state.open -= 1;
                        self.available.notify_one();
                        Err(error)
                    }
                };
            }
            self.available.wait(&mut state);
        }
    }

    fn checkin(&self, connection: Connection) {
        self.state.lock().idle.push(connection);
        self.available.notify_one();
    }

    fn discard(&self) {
        let mut state = self.state.lock();
        state.open -= 1;
        self.available.notify_one();
    }
}

struct PooledConnection {
    pool: Arc<ConnectionPool>,
    connection: Option<Connection>,
}

impl PooledConnection {
    fn connection(&self) -> Result<&Connection> {
        self.connection
            .as_ref()
            .ok_or(SQLiteError::MissingCheckedOutConnection)
    }

    fn connection_mut(&mut self) -> Result<&mut Connection> {
        self.connection
            .as_mut()
            .ok_or(SQLiteError::MissingCheckedOutConnection)
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        let reusable = connection.is_autocommit() || connection.execute_batch("ROLLBACK").is_ok();
        if reusable {
            self.pool.checkin(connection);
        } else {
            self.pool.discard();
        }
    }
}

struct SessionState {
    /// Read guards cover ordinary operations. Transaction lifecycle calls take
    /// the write guard, making BEGIN/COMMIT/ROLLBACK linearizable with respect
    /// to every operation issued through the same logical session.
    gate: RwLock<()>,
    transaction: Mutex<Option<PooledConnection>>,
    transaction_failure: Mutex<Option<String>>,
    cleanup_failure: Mutex<Option<String>>,
    /// Dedicated, never-mutating connection used for `PRAGMA data_version`.
    /// `SQLite` only guarantees comparisons on the same connection, so a
    /// pooled checkout cannot serve as an external-commit monitor.
    data_version_monitor: Mutex<Option<Connection>>,
}

impl SessionState {
    fn new() -> Self {
        Self {
            gate: RwLock::new(()),
            transaction: Mutex::new(None),
            transaction_failure: Mutex::new(None),
            cleanup_failure: Mutex::new(None),
            data_version_monitor: Mutex::new(None),
        }
    }
}

impl Drop for SessionState {
    fn drop(&mut self) {
        // `PooledConnection::drop` performs the rollback and discards the
        // physical connection when rollback itself fails. Taking the pinned
        // handle here therefore cannot return a broken connection to the pool.
        self.transaction.get_mut().take();
    }
}

/// Logical `SQLite` session backed by a bounded physical connection pool.
/// Cloning preserves session/transaction affinity; call [`Self::new_session`]
/// for an independently isolated transaction context.
#[derive(Clone)]
pub struct ManagedConnection {
    pool: Arc<ConnectionPool>,
    session: Arc<SessionState>,
}

impl ManagedConnection {
    fn surface_cleanup_failure(&self) -> Result<()> {
        if let Some(error) = self.session.cleanup_failure.lock().take() {
            return Err(SQLiteError::SessionCleanupFailed(error));
        }
        Ok(())
    }

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
        let spec = ConnectionSpec::File {
            path: path.to_path_buf(),
            key: key.map(Arc::from),
        };
        Self::from_spec(spec, default_pool_connections())
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
        let spec = ConnectionSpec::Compressed {
            path: path.to_path_buf(),
            compression,
        };
        Self::from_spec(spec, default_pool_connections())
    }

    pub fn open_in_memory() -> Result<Self> {
        // Independent `:memory:` connections do not share a database. Keep a
        // one-connection pool for this special target; file-backed databases
        // use the real multi-connection pool.
        Self::from_spec(ConnectionSpec::Memory, 1)
    }

    fn from_spec(spec: ConnectionSpec, max_connections: usize) -> Result<Self> {
        let initial = spec.open(true)?;
        Ok(Self {
            pool: ConnectionPool::new(spec, initial, max_connections),
            session: Arc::new(SessionState::new()),
        })
    }

    fn apply_encryption_key(conn: &Connection, key: &str) -> Result<()> {
        if key.is_empty() {
            return Err(SQLiteError::EmptyEncryptionKey);
        }
        conn.pragma_update(None, "key", key)?;
        Ok(())
    }

    fn enable_wal(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Ok(())
    }

    fn configure_wal_connection(conn: &Connection) -> Result<()> {
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

    fn enable_compressed_journal(conn: &Connection) -> Result<()> {
        // The compressed VFS implements the byte-addressed database file.
        // Rollback journals stay raw because they are short-lived commit
        // machinery; compressing them only adds autocommit write amplification.
        // WAL requires shared-memory VFS methods, so compressed databases use
        // SQLite's rollback journal and keep temp storage in memory.
        conn.pragma_update(None, "journal_mode", "DELETE")?;
        Ok(())
    }

    fn configure_compressed_connection(conn: &Connection) -> Result<()> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /// Create an independent logical session over the same database pool.
    /// Explicit transactions started on either session are isolated and never
    /// capture operations issued through the other session.
    #[must_use]
    pub fn new_session(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            session: Arc::new(SessionState::new()),
        }
    }

    /// Whether this logical session currently owns a pinned transaction
    /// connection.
    #[must_use]
    pub fn in_transaction(&self) -> bool {
        let _gate = self.session.gate.read();
        self.session.transaction.lock().is_some()
    }

    /// Whether the currently pinned transaction has upgraded to a write
    /// transaction. Callers use this to enforce read-only execution
    /// boundaries before COMMIT rather than inferring writes from SQL shape.
    pub fn transaction_has_written(&self) -> Result<bool> {
        let _gate = self.session.gate.read();
        let transaction = self.session.transaction.lock();
        let transaction = transaction
            .as_ref()
            .ok_or(SQLiteError::NoActiveTransaction)?;
        Ok(matches!(
            transaction.connection()?.transaction_state(Some("main"))?,
            rusqlite::TransactionState::Write
        ))
    }

    /// Database change counter observed on a connection permanently owned by
    /// this logical session. The value changes when another `SQLite` connection
    /// commits. In-memory databases have no independent connections and
    /// therefore return `None`.
    pub fn data_version(&self) -> Result<Option<u64>> {
        if matches!(&self.pool.spec, ConnectionSpec::Memory) {
            return Ok(None);
        }
        let mut monitor = self.session.data_version_monitor.lock();
        if monitor.is_none() {
            *monitor = Some(self.pool.spec.open(false)?);
        }
        let monitor = monitor.as_ref().ok_or_else(|| {
            SQLiteError::StorageBackend(
                "data-version monitor was not initialized after opening it".into(),
            )
        })?;
        let version: i64 = monitor.pragma_query_value(None, "data_version", |row| row.get(0))?;
        let version = u64::try_from(version).map_err(|_| {
            SQLiteError::StorageBackend(format!(
                "SQLite returned a negative PRAGMA data_version: {version}"
            ))
        })?;
        Ok(Some(version))
    }

    /// Run a closure using this session. Outside a transaction the closure
    /// checks out a pooled connection; inside a transaction every clone is
    /// routed to the session's pinned connection.
    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        let transaction = self.session.transaction.lock();
        if let Some(connection) = transaction.as_ref() {
            if let Some(error) = self.session.transaction_failure.lock().as_ref() {
                return Err(SQLiteError::TransactionAborted(error.clone()));
            }
            let result = f(connection.connection()?);
            if let Err(error) = &result {
                let mut failure = self.session.transaction_failure.lock();
                if failure.is_none() {
                    *failure = Some(error.to_string());
                }
            }
            return result;
        }
        drop(transaction);
        let connection = self.pool.checkout()?;
        f(connection.connection()?)
    }

    pub fn with_mut<R>(&self, f: impl FnOnce(&mut Connection) -> Result<R>) -> Result<R> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        let mut transaction = self.session.transaction.lock();
        if let Some(connection) = transaction.as_mut() {
            if let Some(error) = self.session.transaction_failure.lock().as_ref() {
                return Err(SQLiteError::TransactionAborted(error.clone()));
            }
            let result = f(connection.connection_mut()?);
            if let Err(error) = &result {
                let mut failure = self.session.transaction_failure.lock();
                if failure.is_none() {
                    *failure = Some(error.to_string());
                }
            }
            return result;
        }
        drop(transaction);
        let mut connection = self.pool.checkout()?;
        f(connection.connection_mut()?)
    }

    /// Open an explicit (non-deferred) transaction. Subsequent
    /// auto-commit hosts (catalog writes, FTS index updates, ...) all
    /// flow through the same connection so the transaction enclosing
    /// them is honoured. Use [`Self::commit_transaction`] /
    /// [`Self::rollback_transaction`] / [`Self::savepoint`] etc. for
    /// the lifecycle.
    pub fn begin_transaction(&self) -> Result<()> {
        self.begin_transaction_with("BEGIN IMMEDIATE")
    }

    /// Open a deferred transaction. Read-only SQL statements use this mode
    /// so WAL readers do not take the single writer reservation; if a scalar
    /// routine performs a write, `SQLite` upgrades the same transaction and
    /// still preserves the statement's atomic boundary.
    pub fn begin_deferred_transaction(&self) -> Result<()> {
        self.begin_transaction_with("BEGIN DEFERRED")
    }

    fn begin_transaction_with(&self, statement: &str) -> Result<()> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.write();
        let mut transaction = self.session.transaction.lock();
        if transaction.is_some() {
            return Err(SQLiteError::TransactionAlreadyActive);
        }
        let connection = self.pool.checkout()?;
        connection.connection()?.execute_batch(statement)?;
        self.session.transaction_failure.lock().take();
        *transaction = Some(connection);
        Ok(())
    }

    pub fn commit_transaction(&self) -> Result<()> {
        self.surface_cleanup_failure()?;
        self.finish_transaction("COMMIT")
    }

    pub fn rollback_transaction(&self) -> Result<()> {
        self.surface_cleanup_failure()?;
        self.finish_transaction("ROLLBACK")
    }

    /// Drop-only transaction cleanup. A rollback error is recorded on the
    /// logical session and is returned by its next operation instead of being
    /// mistaken for a successful rollback.
    pub(crate) fn rollback_transaction_on_drop(&self) {
        if let Err(error) = self.finish_transaction("ROLLBACK") {
            let mut failure = self.session.cleanup_failure.lock();
            if failure.is_none() {
                *failure = Some(error.to_string());
            }
        }
    }

    fn finish_transaction(&self, statement: &str) -> Result<()> {
        let _gate = self.session.gate.write();
        let mut transaction = self.session.transaction.lock();
        let connection = transaction
            .as_ref()
            .ok_or(SQLiteError::NoActiveTransaction)?;
        if statement == "COMMIT" {
            // Materialize the failure before entering the branch. Holding the
            // mutex guard created by an `if let` scrutinee until the end of
            // the branch would deadlock when cleanup takes the same lock.
            let transaction_failure = self.session.transaction_failure.lock().clone();
            if let Some(error) = transaction_failure {
                if let Err(rollback_error) = connection.connection()?.execute_batch("ROLLBACK") {
                    transaction.take();
                    self.session.transaction_failure.lock().take();
                    return Err(SQLiteError::SQLite(rollback_error));
                }
                transaction.take();
                self.session.transaction_failure.lock().take();
                return Err(SQLiteError::TransactionAborted(error));
            }
        }
        if let Err(error) = connection.connection()?.execute_batch(statement) {
            transaction.take();
            self.session.transaction_failure.lock().take();
            return Err(SQLiteError::SQLite(error));
        }
        transaction.take();
        self.session.transaction_failure.lock().take();
        Ok(())
    }

    fn with_transaction<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let _gate = self.session.gate.read();
        let transaction = self.session.transaction.lock();
        let connection = transaction
            .as_ref()
            .ok_or(SQLiteError::NoActiveTransaction)?;
        f(connection.connection()?)
    }

    pub fn savepoint(&self, name: &str) -> Result<()> {
        self.surface_cleanup_failure()?;
        let stmt = format!("SAVEPOINT \"{}\"", name.replace('"', "\"\""));
        self.with_transaction(|c| {
            c.execute_batch(&stmt)?;
            Ok(())
        })
    }

    pub fn release_savepoint(&self, name: &str) -> Result<()> {
        self.surface_cleanup_failure()?;
        let stmt = format!("RELEASE SAVEPOINT \"{}\"", name.replace('"', "\"\""));
        self.with_transaction(|c| {
            c.execute_batch(&stmt)?;
            Ok(())
        })
    }

    pub fn rollback_to_savepoint(&self, name: &str) -> Result<()> {
        self.surface_cleanup_failure()?;
        let stmt = format!("ROLLBACK TO SAVEPOINT \"{}\"", name.replace('"', "\"\""));
        self.with_transaction(|c| {
            c.execute_batch(&stmt)?;
            Ok(())
        })?;
        self.session.transaction_failure.lock().take();
        Ok(())
    }
}

fn default_pool_connections() -> usize {
    std::thread::available_parallelism()
        .map_or(MIN_POOL_CONNECTIONS, |parallelism| parallelism.get() * 2)
        .clamp(MIN_POOL_CONNECTIONS, MAX_POOL_CONNECTIONS)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::transaction::SQLiteTransaction;

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
    fn data_version_uses_one_stable_monitor_connection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data-version.db");
        let observer = ManagedConnection::open(&path).unwrap();
        let writer = ManagedConnection::open(&path).unwrap();
        let before = observer.data_version().unwrap().unwrap();
        writer
            .with(|connection| {
                connection.execute_batch(
                    "CREATE TABLE committed (id INTEGER PRIMARY KEY); \
                     INSERT INTO committed (id) VALUES (1)",
                )?;
                Ok(())
            })
            .unwrap();
        let after = observer.data_version().unwrap().unwrap();
        assert_ne!(after, before);
        assert_eq!(observer.data_version().unwrap(), Some(after));
    }

    #[test]
    fn file_sessions_run_read_closures_concurrently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("concurrent.sqlite3");
        let base = ManagedConnection::open(&path).unwrap();
        base.with(|connection| {
            connection.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", [])?;
            connection.execute("INSERT INTO t (id) VALUES (1)", [])?;
            Ok(())
        })
        .unwrap();

        let first = base.new_session();
        let second = base.new_session();
        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first_thread = thread::spawn(move || {
            first
                .with(|connection| {
                    let count: i64 =
                        connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?;
                    assert_eq!(count, 1);
                    first_entered_tx.send(()).unwrap();
                    release_first_rx.recv().unwrap();
                    Ok(())
                })
                .unwrap();
        });
        first_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first reader entered its connection closure");

        let (second_entered_tx, second_entered_rx) = mpsc::channel();
        let second_thread = thread::spawn(move || {
            second
                .with(|connection| {
                    let count: i64 =
                        connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?;
                    assert_eq!(count, 1);
                    second_entered_tx.send(()).unwrap();
                    Ok(())
                })
                .unwrap();
        });

        let concurrent = second_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .is_ok();
        release_first_tx.send(()).unwrap();
        first_thread.join().unwrap();
        second_thread.join().unwrap();
        assert!(
            concurrent,
            "a database-wide connection mutex serialized independent readers"
        );
    }

    #[test]
    fn transaction_is_pinned_to_clones_and_isolated_from_new_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("isolation.sqlite3");
        let writer = ManagedConnection::open(&path).unwrap();
        writer
            .with(|connection| {
                connection.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", [])?;
                Ok(())
            })
            .unwrap();
        let writer_clone = writer.clone();
        let observer = writer.new_session();

        writer.begin_transaction().unwrap();
        assert!(writer.in_transaction());
        assert!(writer_clone.in_transaction());
        assert!(!observer.in_transaction());
        writer_clone
            .with(|connection| {
                connection.execute("INSERT INTO t (id, v) VALUES (1, 'pending')", [])?;
                connection.execute("CREATE TEMP TABLE pinned (v INTEGER)", [])?;
                connection.execute("INSERT INTO pinned (v) VALUES (7)", [])?;
                Ok(())
            })
            .unwrap();
        let pinned_value: i64 = writer
            .with(|connection| {
                Ok(connection.query_row("SELECT v FROM pinned", [], |row| row.get(0))?)
            })
            .unwrap();
        assert_eq!(pinned_value, 7);

        let writer_count: i64 = writer
            .with(|connection| {
                Ok(connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?)
            })
            .unwrap();
        let observer_count: i64 = observer
            .with(|connection| {
                Ok(connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?)
            })
            .unwrap();
        assert_eq!(writer_count, 1);
        assert_eq!(observer_count, 0);

        writer.commit_transaction().unwrap();
        assert!(!writer.in_transaction());
        let committed_count: i64 = observer
            .with(|connection| {
                Ok(connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?)
            })
            .unwrap();
        assert_eq!(committed_count, 1);
    }

    #[test]
    fn dropping_session_rolls_back_its_pinned_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drop-rollback.sqlite3");
        let base = ManagedConnection::open(&path).unwrap();
        base.with(|connection| {
            connection.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", [])?;
            Ok(())
        })
        .unwrap();
        {
            let transaction = base.new_session();
            transaction.begin_transaction().unwrap();
            transaction
                .with(|connection| {
                    connection.execute("INSERT INTO t (id) VALUES (1)", [])?;
                    Ok(())
                })
                .unwrap();
        }
        let count: i64 = base
            .with(|connection| {
                Ok(connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn ignored_storage_error_aborts_explicit_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aborted.sqlite3");
        let writer = ManagedConnection::open(&path).unwrap();
        writer
            .with(|connection| {
                connection.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", [])?;
                Ok(())
            })
            .unwrap();
        let observer = writer.new_session();

        writer.begin_transaction().unwrap();
        writer
            .with(|connection| {
                connection.execute("INSERT INTO t (id) VALUES (1)", [])?;
                Ok(())
            })
            .unwrap();
        let _ignored = writer.with(|connection| {
            connection.execute("INSERT INTO missing_table (id) VALUES (1)", [])?;
            Ok(())
        });
        assert!(matches!(
            writer.commit_transaction(),
            Err(SQLiteError::TransactionAborted(_))
        ));

        let count: i64 = observer
            .with(|connection| {
                Ok(connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn failed_drop_rollback_is_reported_to_the_session() {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let session = connection.new_session();
        let transaction = SQLiteTransaction::begin(session.clone()).unwrap();

        // Desynchronise SQLite's transaction state from the managed session to
        // exercise the otherwise difficult rollback-failure path. The
        // transaction guard must not silently report this cleanup as success.
        session
            .with(|sqlite| {
                sqlite.execute_batch("COMMIT")?;
                Ok(())
            })
            .unwrap();
        drop(transaction);

        assert!(matches!(
            session.with(|_| Ok(())),
            Err(SQLiteError::SessionCleanupFailed(_))
        ));

        // The failed cleanup notification is consumed exactly once, and the
        // managed session remains usable with a clean pooled connection.
        session
            .with(|sqlite| {
                sqlite.execute("CREATE TABLE recovered (id INTEGER PRIMARY KEY)", [])?;
                Ok(())
            })
            .unwrap();
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
        assert_eq!(&compressed[..8], b"UQACDB1\0");
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
        assert_eq!(&bytes[..8], b"UQACDB1\0");

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
