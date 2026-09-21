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
use std::sync::{Arc, OnceLock};

use parking_lot::{Condvar, Mutex, RwLock};
use rusqlite::{Connection, OpenFlags};
use uqa_storage::{mvcc::VersionedKeyValueStore, KeyValueStore, StorageEncryptionKey};

use crate::compressed_vfs::{self, SQLiteCompressedContainerAnchor, SQLiteCompressionOptions};

mod identity;
mod logical;
mod native;
mod native_restore;
mod serializable;
mod snapshot;
use snapshot::PhysicalConnection;
pub(crate) use snapshot::SnapshotIdentity;

#[derive(Debug, thiserror::Error)]
pub enum SQLiteError {
    #[error(transparent)]
    Memory(#[from] uqa_core::memory::MemoryError),
    #[error(transparent)]
    Cancelled(#[from] uqa_core::QueryCancelled),
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
    #[error("auxiliary database requires DELETE journaling, found {0}")]
    AuxiliaryJournalMode(String),
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
    #[error(transparent)]
    StorageSource(Box<uqa_storage::StorageBackendError>),
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
    #[error("versioned storage requires its logical session; use with_physical only for explicit physical maintenance")]
    LogicalSessionRequired,
    #[error("the SQLite session already has a different retention limit")]
    SessionOptionsMismatch,
    #[error("the SQLite session is bound to a different record mapping")]
    SessionMappingMismatch,
}

pub type Result<T> = std::result::Result<T, SQLiteError>;

const MIN_POOL_CONNECTIONS: usize = 4;
const MAX_POOL_CONNECTIONS: usize = 32;

#[derive(Clone)]
enum ConnectionSpec {
    File {
        path: PathBuf,
        key: Option<StorageEncryptionKey>,
    },
    Auxiliary {
        path: PathBuf,
        key: Option<StorageEncryptionKey>,
    },
    Compressed {
        path: PathBuf,
        compression: SQLiteCompressionOptions,
        key: Option<StorageEncryptionKey>,
    },
    Memory,
}

impl ConnectionSpec {
    fn open(&self, initialize_database: bool) -> Result<Connection> {
        match self {
            Self::File { path, key } | Self::Auxiliary { path, key } => {
                let conn = Connection::open(path)?;
                if let Some(key) = key {
                    ManagedConnection::apply_encryption_key(&conn, key.expose_secret())?;
                }
                if matches!(self, Self::Auxiliary { .. }) {
                    // Registry writers already serialize their transactions.
                    // Retain the original journal mode instead of racing to
                    // promote a new registry to WAL during concurrent opens.
                    let mode: String =
                        conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
                    if mode != "delete" {
                        return Err(SQLiteError::AuxiliaryJournalMode(mode));
                    }
                    ManagedConnection::configure_rollback_connection(&conn)?;
                } else {
                    if initialize_database {
                        ManagedConnection::enable_wal(&conn)?;
                    }
                    ManagedConnection::configure_wal_connection(&conn)?;
                }
                Ok(conn)
            }
            Self::Compressed {
                path, compression, ..
            } => {
                let conn = Connection::open_with_flags_and_vfs(
                    path,
                    OpenFlags::default(),
                    compressed_vfs::VFS_NAME,
                )?;
                if initialize_database {
                    conn.pragma_update(None, "page_size", compression.page_size)?;
                    ManagedConnection::enable_compressed_journal(&conn)?;
                }
                ManagedConnection::configure_rollback_connection(&conn)?;
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
    idle: Vec<PhysicalConnection>,
    open: usize,
}

struct ConnectionPool {
    memory_identity: Mutex<Option<String>>,
    serializable_leases: Mutex<Option<Arc<uqa_storage::mvcc::LocalSerializableLeases>>>,
    serializable_connection: Mutex<Option<(uqa_storage::mvcc::DatabaseId, ManagedConnection)>>,
    snapshot_registry: Mutex<
        Option<(
            uqa_storage::mvcc::DatabaseId,
            std::sync::Weak<uqa_storage::mvcc::SnapshotRegistry>,
        )>,
    >,
    spec: ConnectionSpec,
    max_connections: usize,
    state: Mutex<PoolState>,
    available: Condvar,
    /// Stable, never-mutating connection used for `PRAGMA data_version`.
    /// Every logical session over this pool must compare versions on this
    /// same connection, and encrypted databases must not repeat key
    /// derivation merely to create a request-local change monitor.
    data_version_monitor: Mutex<Option<Connection>>,
}

impl ConnectionPool {
    fn new(spec: ConnectionSpec, initial: Connection, max_connections: usize) -> Arc<Self> {
        Arc::new(Self {
            memory_identity: Mutex::new(None),
            serializable_leases: Mutex::new(None),
            serializable_connection: Mutex::new(None),
            snapshot_registry: Mutex::new(None),
            spec,
            max_connections: max_connections.max(1),
            state: Mutex::new(PoolState {
                idle: vec![PhysicalConnection::new(initial)],
                open: 1,
            }),
            available: Condvar::new(),
            data_version_monitor: Mutex::new(None),
        })
    }

    fn checkout(self: &Arc<Self>) -> Result<PooledConnection> {
        self.checkout_with_cancellation(None)
    }

    fn checkout_with_cancellation(
        self: &Arc<Self>,
        cancellation: Option<&uqa_core::CancellationToken>,
    ) -> Result<PooledConnection> {
        loop {
            if let Some(cancellation) = cancellation {
                cancellation.check()?;
            }
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
                        connection: Some(PhysicalConnection::new(connection)),
                    }),
                    Err(error) => {
                        let mut state = self.state.lock();
                        state.open -= 1;
                        self.available.notify_one();
                        Err(error)
                    }
                };
            }
            if cancellation.is_some() {
                self.available
                    .wait_for(&mut state, std::time::Duration::from_millis(10));
            } else {
                self.available.wait(&mut state);
            }
        }
    }

    fn checkin(&self, connection: PhysicalConnection) {
        self.state.lock().idle.push(connection);
        self.available.notify_one();
    }

    fn discard(&self) {
        let mut state = self.state.lock();
        state.open -= 1;
        self.available.notify_one();
    }
}

pub(crate) struct PooledConnection {
    pool: Arc<ConnectionPool>,
    connection: Option<PhysicalConnection>,
}

impl PooledConnection {
    pub(crate) fn connection(&self) -> Result<&Connection> {
        self.connection
            .as_ref()
            .map(|physical| &physical.connection)
            .ok_or(SQLiteError::MissingCheckedOutConnection)
    }

    fn physical_mut(&mut self) -> Result<&mut PhysicalConnection> {
        self.connection
            .as_mut()
            .ok_or(SQLiteError::MissingCheckedOutConnection)
    }

    pub(crate) fn connection_mut(&mut self) -> Result<&mut Connection> {
        self.physical_mut().map(|physical| &mut physical.connection)
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        let reusable = connection.connection.is_autocommit()
            || connection.connection.execute_batch("ROLLBACK").is_ok();
        if reusable {
            self.pool.checkin(connection);
        } else {
            self.pool.discard();
        }
    }
}

struct SessionState {
    write_cancellation: uqa_core::CancellationToken,
    affinity: uqa_storage::StorageSessionAffinity,
    /// Read guards cover ordinary operations. Transaction lifecycle calls take
    /// the write guard, making BEGIN/COMMIT/ROLLBACK linearizable with respect
    /// to every operation issued through the same logical session.
    gate: RwLock<()>,
    transaction: Mutex<Option<PooledConnection>>,
    transaction_failure: Mutex<Option<String>>,
    cleanup_failure: Mutex<Option<String>>,
    logical: OnceLock<Arc<logical::BoundRecordSession>>,
    native_restore: Mutex<Option<native_restore::NativeRestore>>,
    snapshot_branch: Mutex<Arc<()>>,
}

impl SessionState {
    fn new() -> Self {
        Self::with_cancellation(uqa_core::CancellationToken::new())
    }

    fn with_cancellation(write_cancellation: uqa_core::CancellationToken) -> Self {
        Self {
            write_cancellation,
            affinity: uqa_storage::StorageSessionAffinity::new(),
            gate: RwLock::new(()),
            transaction: Mutex::new(None),
            transaction_failure: Mutex::new(None),
            cleanup_failure: Mutex::new(None),
            logical: OnceLock::new(),
            native_restore: Mutex::new(None),
            snapshot_branch: Mutex::new(Arc::new(())),
        }
    }
}

impl Drop for SessionState {
    fn drop(&mut self) {
        // `PooledConnection::drop` performs the rollback and discards the
        // physical connection when rollback itself fails. Taking the pinned
        // handle here therefore cannot return a broken connection to the pool.
        self.native_restore.get_mut().take();
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
    record_access: bool,
}

impl ManagedConnection {
    fn surface_cleanup_failure(&self) -> Result<()> {
        if let Some(error) = self.session.cleanup_failure.lock().take() {
            return Err(SQLiteError::SessionCleanupFailed(error));
        }
        Ok(())
    }

    pub fn open(path: &Path) -> Result<Self> {
        if path == Path::new(":memory:") {
            return Self::open_in_memory();
        }
        Self::open_with_optional_key(path, None)
    }

    /// Retain the database credential for encrypted auxiliary storage, including
    /// compressed containers whose main-file encryption lives in the VFS.
    #[must_use]
    pub fn auxiliary_encryption_key(&self) -> Option<StorageEncryptionKey> {
        match &self.pool.spec {
            ConnectionSpec::File { key, .. }
            | ConnectionSpec::Auxiliary { key, .. }
            | ConnectionSpec::Compressed { key, .. } => key.clone(),
            ConnectionSpec::Memory => None,
        }
    }

    /// Lease an independent physical connection without joining this logical
    /// session's transaction. The pool retains encryption and connection policy.
    pub fn lease_connection(&self) -> Result<crate::SQLiteConnectionLease> {
        self.pool.checkout().map(crate::SQLiteConnectionLease)
    }

    /// Open database-owned auxiliary storage with its inherited credential and
    /// original DELETE journaling. Concurrent first opens never change journal
    /// modes. An incompatible existing mode is rejected without rewriting it.
    pub fn open_auxiliary(path: &Path, key: Option<StorageEncryptionKey>) -> Result<Self> {
        if path == Path::new(":memory:") || path.as_os_str().is_empty() {
            return Err(SQLiteError::StorageBackend(
                "auxiliary storage requires a database file".into(),
            ));
        }
        Self::from_spec(
            ConnectionSpec::Auxiliary {
                path: path.to_path_buf(),
                key,
            },
            default_pool_connections(),
        )
    }

    pub fn open_encrypted(path: &Path, key: &str) -> Result<Self> {
        Self::open_with_optional_key(path, Some(key))
    }

    pub fn open_compressed(path: &Path, compression: SQLiteCompressionOptions) -> Result<Self> {
        Self::open_compressed_with_optional_key(path, compression, None, None)
    }

    pub fn open_compressed_encrypted(
        path: &Path,
        key: &str,
        compression: SQLiteCompressionOptions,
    ) -> Result<Self> {
        if key.is_empty() {
            return Err(SQLiteError::EmptyEncryptionKey);
        }
        Self::open_compressed_with_optional_key(path, compression, Some(key), None)
    }

    /// Open an encrypted compressed database while enforcing an exact trusted
    /// external state anchor in the VFS main-file open path.
    pub fn open_compressed_encrypted_with_anchor(
        path: &Path,
        key: &str,
        compression: SQLiteCompressionOptions,
        trusted_anchor: SQLiteCompressedContainerAnchor,
    ) -> Result<Self> {
        if key.is_empty() {
            return Err(SQLiteError::EmptyEncryptionKey);
        }
        Self::open_compressed_with_optional_key(path, compression, Some(key), Some(trusted_anchor))
    }

    fn open_with_optional_key(path: &Path, key: Option<&str>) -> Result<Self> {
        let spec = ConnectionSpec::File {
            path: path.to_path_buf(),
            key: key.map(StorageEncryptionKey::new),
        };
        Self::from_spec(spec, default_pool_connections())
    }

    fn open_compressed_with_optional_key(
        path: &Path,
        compression: SQLiteCompressionOptions,
        key: Option<&str>,
        trusted_anchor: Option<SQLiteCompressedContainerAnchor>,
    ) -> Result<Self> {
        let compression = compression
            .validate()
            .map_err(SQLiteError::CompressedContainer)?;
        match trusted_anchor {
            Some(anchor) => compressed_vfs::register_database_with_anchor(
                path,
                compression,
                key.ok_or(SQLiteError::EncryptionKeyRequired)?,
                anchor,
            ),
            None => compressed_vfs::register_database(path, compression, key),
        }
        .map_err(SQLiteError::CompressedContainer)?;
        let spec = ConnectionSpec::Compressed {
            path: path.to_path_buf(),
            compression,
            key: key.map(StorageEncryptionKey::new),
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
            record_access: false,
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
        // Legacy physical operations retain a bounded busy wait. Versioned writes supply cancellable admission under a scoped timeout.
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

    fn configure_rollback_connection(conn: &Connection) -> Result<()> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /// Identity shared by handles using this session's native or logical transaction context.
    pub fn transaction_affinity(&self) -> uqa_storage::StorageSessionAffinity {
        let _gate = self.session.gate.read();
        self.session.logical.get().map_or_else(
            || self.session.affinity.clone(),
            |logical| logical.session_affinity(),
        )
    }

    /// Whether this session has an active native or logical record transaction.
    #[must_use]
    pub fn in_transaction(&self) -> bool {
        let _gate = self.session.gate.read();
        if let Some(logical) = self.session.logical.get() {
            return logical.in_transaction();
        }
        self.session.transaction.lock().is_some()
    }

    /// Whether this session has staged writes, including private logical records. Callers use this to enforce read-only execution boundaries before COMMIT.
    pub fn transaction_has_written(&self) -> Result<bool> {
        let _gate = self.session.gate.read();
        if let Some(logical) = self.session.logical.get() {
            return logical.transaction_has_written().map_err(Into::into);
        }
        let transaction = self.session.transaction.lock();
        let transaction = transaction
            .as_ref()
            .ok_or(SQLiteError::NoActiveTransaction)?;
        Ok(matches!(
            transaction.connection()?.transaction_state(Some("main"))?,
            rusqlite::TransactionState::Write
        ))
    }

    /// Whether this session's independent [`Self::data_version`] monitor can
    /// read without contending with the currently pinned transaction.
    ///
    /// A rollback-journal writer's pending lock blocks new readers while waiting for existing readers to finish. A rollback-journal read transaction must therefore also avoid the independent monitor: its own shared lock may be preventing that waiting writer from proceeding. Callers refresh through the pinned connection instead. WAL sessions and sessions without a pinned transaction permit the independent monitor.
    pub fn data_version_monitor_is_nonblocking(&self) -> Result<bool> {
        let _gate = self.session.gate.read();
        if self.session.logical.get().is_some() {
            return Ok(true);
        }
        if !matches!(
            &self.pool.spec,
            ConnectionSpec::Compressed { .. } | ConnectionSpec::Auxiliary { .. }
        ) {
            return Ok(true);
        }
        Ok(self.session.transaction.lock().is_none())
    }

    /// Whether one pooled connection may retain a read snapshot while another writes. Ordinary plain and encrypted `SQLite` databases use WAL; compressed containers and auxiliary files use rollback journaling and therefore require a detached engine snapshot before writer promotion.
    #[must_use]
    pub fn supports_concurrent_pinned_read_and_write(&self) -> bool {
        let _gate = self.session.gate.read();
        self.session.logical.get().is_some()
            || !matches!(
                &self.pool.spec,
                ConnectionSpec::Compressed { .. } | ConnectionSpec::Auxiliary { .. }
            )
    }

    /// Database change counter observed on one stable connection shared by
    /// every logical session over this pool. The value changes when another
    /// `SQLite` connection commits. In-memory databases have no independent
    /// connections and therefore return `None`.
    pub fn data_version(&self) -> Result<Option<u64>> {
        let _gate = self.session.gate.read();
        if let Some(logical) = self.session.logical.get() {
            return logical.change_version().map_err(Into::into);
        }
        if matches!(&self.pool.spec, ConnectionSpec::Memory) {
            return Ok(None);
        }
        let mut monitor = self.pool.data_version_monitor.lock();
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

    /// Establish the database snapshot for the active transaction without
    /// depending on the caller's first user query. `BEGIN DEFERRED` alone does
    /// not start a read transaction, so a writer could otherwise commit after
    /// the engine checks its cache generations but before the first catalog
    /// read. Reading `sqlite_schema` is database-wide and keeps the operation
    /// independent of any application table.
    pub fn pin_transaction_snapshot(&self) -> Result<()> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        if let Some(logical) = self.session.logical.get() {
            return if logical.in_transaction() {
                Ok(())
            } else {
                Err(SQLiteError::NoActiveTransaction)
            };
        }
        self.with_native(|connection| {
            if connection.is_autocommit() {
                return Err(SQLiteError::NoActiveTransaction);
            }
            let _: i64 =
                connection.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get(0))?;
            Ok(())
        })
    }

    /// Run a closure using this session. Outside a transaction the closure
    /// checks out a pooled connection; inside a transaction every clone is
    /// routed to the session's pinned connection.
    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        if self.session.logical.get().is_some() {
            return Err(SQLiteError::LogicalSessionRequired);
        }
        self.with_native(f)
    }

    /// The caller holds the session gate and has ruled out logical record access.
    fn with_native<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let transaction = self.session.transaction.lock();
        if let Some(connection) = transaction.as_ref() {
            if let Some(error) = self.session.transaction_failure.lock().as_ref() {
                return Err(SQLiteError::TransactionAborted(error.clone()));
            }
            self.check_native_access(connection.connection()?)?;
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
        self.check_native_access(connection.connection()?)?;
        f(connection.connection()?)
    }

    pub fn with_mut<R>(&self, f: impl FnOnce(&mut Connection) -> Result<R>) -> Result<R> {
        self.with_physical_mut(|physical| f(&mut physical.connection))
    }

    fn with_physical_mut<R>(
        &self,
        f: impl FnOnce(&mut PhysicalConnection) -> Result<R>,
    ) -> Result<R> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        if self.session.logical.get().is_some() {
            return Err(SQLiteError::LogicalSessionRequired);
        }
        let mut transaction = self.session.transaction.lock();
        if let Some(connection) = transaction.as_mut() {
            if let Some(error) = self.session.transaction_failure.lock().as_ref() {
                return Err(SQLiteError::TransactionAborted(error.clone()));
            }
            self.check_native_access(connection.connection()?)?;
            let result = f(connection.physical_mut()?);
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
        self.check_native_access(connection.connection()?)?;
        f(connection.physical_mut()?)
    }

    /// Rewrite the `SQLite` database into its minimum-sized file. `SQLite` requires `VACUUM` to run in autocommit mode, so the session write gate makes the transaction check and maintenance command one atomic session operation.
    pub fn vacuum(&self) -> Result<()> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.write();
        if self.session.transaction.lock().is_some()
            || self
                .session
                .logical
                .get()
                .is_some_and(|logical| logical.in_transaction())
        {
            return Err(SQLiteError::TransactionAlreadyActive);
        }
        if let Some(logical) = self.session.logical.get() {
            logical.reclaim_versions()?;
        }
        let connection = self.pool.checkout()?;
        connection.connection()?.execute_batch("VACUUM")?;
        Ok(())
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
        if let Some(logical) = self.session.logical.get() {
            if logical.in_transaction() {
                return Err(SQLiteError::TransactionAlreadyActive);
            }
            return if statement == "BEGIN DEFERRED" {
                logical.begin_upgradeable_transaction()
            } else {
                logical.begin_transaction()
            }
            .map_err(Into::into);
        }
        let mut transaction = self.session.transaction.lock();
        if transaction.is_some() {
            return Err(SQLiteError::TransactionAlreadyActive);
        }
        let connection = self.pool.checkout()?;
        self.check_native_access(connection.connection()?)?;
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
        if let Some(logical) = self.session.logical.get() {
            if !logical.in_transaction() {
                return Err(SQLiteError::NoActiveTransaction);
            }
            return if statement == "COMMIT" {
                logical.commit_transaction()
            } else {
                logical.rollback_transaction()
            }
            .map_err(Into::into);
        }
        let mut transaction = self.session.transaction.lock();
        let connection = transaction
            .as_ref()
            .ok_or(SQLiteError::NoActiveTransaction)?;
        *self.session.snapshot_branch.lock() = Arc::new(());
        if statement == "COMMIT" {
            // Materialize the failure before entering the branch. Holding the
            // mutex guard created by an `if let` scrutinee until the end of
            // the branch would deadlock when cleanup takes the same lock.
            let transaction_failure = self.session.transaction_failure.lock().clone();
            if let Some(error) = transaction_failure {
                if let Err(rollback_error) = connection.connection()?.execute_batch("ROLLBACK") {
                    self.session.native_restore.lock().take();
                    transaction.take();
                    self.session.transaction_failure.lock().take();
                    return Err(SQLiteError::SQLite(rollback_error));
                }
                self.session.native_restore.lock().take();
                transaction.take();
                self.session.transaction_failure.lock().take();
                return Err(SQLiteError::TransactionAborted(error));
            }
        }
        let native = if statement == "COMMIT" && self.session.native_restore.lock().is_some() {
            match self.prepare_initial_native_binding(connection.connection()?) {
                Ok(native) => Some(native),
                Err(error) => {
                    *self.session.transaction_failure.lock() = Some(error.to_string());
                    return Err(error);
                }
            }
        } else {
            None
        };
        if let Err(error) = connection.connection()?.execute_batch(statement) {
            self.session.native_restore.lock().take();
            transaction.take();
            self.session.transaction_failure.lock().take();
            return Err(SQLiteError::SQLite(error));
        }
        self.session.native_restore.lock().take();
        transaction.take();
        self.session.transaction_failure.lock().take();
        if let Some(native) = native {
            self.install_initial_native_binding(native);
        }
        Ok(())
    }

    fn with_transaction<R>(
        &self,
        logical: impl FnOnce(&VersionedKeyValueStore) -> uqa_storage::StorageBackendResult<R>,
        native: impl FnOnce(&Connection) -> Result<R>,
    ) -> Result<R> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        if let Some(store) = self.session.logical.get() {
            if !store.in_transaction() {
                return Err(SQLiteError::NoActiveTransaction);
            }
            return logical(store).map_err(Into::into);
        }
        let transaction = self.session.transaction.lock();
        let connection = transaction
            .as_ref()
            .ok_or(SQLiteError::NoActiveTransaction)?;
        self.check_native_access(connection.connection()?)?;
        native(connection.connection()?)
    }

    pub fn savepoint(&self, name: &str) -> Result<()> {
        self.with_transaction(
            |logical| logical.savepoint(name),
            |connection| {
                let stmt = format!("SAVEPOINT \"{}\"", name.replace('"', "\"\""));
                connection.execute_batch(&stmt)?;
                Ok(())
            },
        )
    }

    pub fn release_savepoint(&self, name: &str) -> Result<()> {
        self.with_transaction(
            |logical| logical.release_savepoint(name),
            |connection| {
                let stmt = format!("RELEASE SAVEPOINT \"{}\"", name.replace('"', "\"\""));
                connection.execute_batch(&stmt)?;
                Ok(())
            },
        )
    }

    pub fn rollback_to_savepoint(&self, name: &str) -> Result<()> {
        self.with_transaction(
            |logical| logical.rollback_to_savepoint(name),
            |connection| {
                let stmt = format!("ROLLBACK TO SAVEPOINT \"{}\"", name.replace('"', "\"\""));
                connection.execute_batch(&stmt)?;
                self.session.transaction_failure.lock().take();
                *self.session.snapshot_branch.lock() = Arc::new(());
                Ok(())
            },
        )
    }
}

fn default_pool_connections() -> usize {
    std::thread::available_parallelism()
        .map_or(MIN_POOL_CONNECTIONS, |parallelism| parallelism.get() * 2)
        .clamp(MIN_POOL_CONNECTIONS, MAX_POOL_CONNECTIONS)
}

#[cfg(test)]
mod tests;

impl From<uqa_storage::mvcc::VersionError> for SQLiteError {
    fn from(error: uqa_storage::mvcc::VersionError) -> Self {
        Self::from(error.into_storage_error())
    }
}

impl From<SQLiteError> for uqa_storage::StorageBackendError {
    fn from(source: SQLiteError) -> Self {
        match source {
            SQLiteError::Memory(error) => Self::Memory(error),
            SQLiteError::Cancelled(error) => Self::Cancelled(error),
            SQLiteError::StorageSource(error) => *error,
            source => Self::backend("SQLite", source),
        }
    }
}

impl From<SQLiteError> for uqa_storage::TransactionError {
    fn from(source: SQLiteError) -> Self {
        Self::Storage(source.into())
    }
}

impl From<uqa_storage::StorageBackendError> for SQLiteError {
    fn from(error: uqa_storage::StorageBackendError) -> Self {
        use uqa_storage::StorageBackendError;
        match error {
            StorageBackendError::Memory(error) => Self::Memory(error),
            StorageBackendError::Cancelled(error) => Self::Cancelled(error),
            StorageBackendError::Analysis(error) => Self::Analysis(error),
            StorageBackendError::Serde(error) => Self::Serde(error),
            StorageBackendError::Backend { backend, source } => match source.downcast::<Self>() {
                Ok(error) => *error,
                Err(source) => {
                    Self::StorageSource(Box::new(StorageBackendError::Backend { backend, source }))
                }
            },
            StorageBackendError::Other(message) => Self::StorageBackend(message),
        }
    }
}
