//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! File-format opening, connection binding, and independent session creation.

use super::{
    Arc, Catalog, CatalogFacade, DeepModel, Engine, ManagedConnection, Path,
    PersistentStorageBackend, SQLiteCompressedContainerAnchor, SQLiteCompressionOptions,
    SQLiteError, SQLiteStorageBackend, StorageBackendError, StorageBackendResult,
};

impl Engine {
    pub fn open(path: &Path) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open(path)?;
        Self::open_with_connection(&conn)
    }

    /// Classify the on-disk format of `path` without opening it: plain
    /// `SQLite`, UQA compressed container (with its encryption flag), a
    /// missing/empty file, or an unrecognized header (`SQLCipher`
    /// databases fall here because `SQLCipher` encrypts the whole file).
    pub fn detect_database_file(path: &Path) -> std::io::Result<uqa_storage::DatabaseFileFormat> {
        uqa_storage::detect_database_file_format(path)
    }

    /// Open `path` with the variant its on-disk format calls for.
    ///
    /// - Missing/empty file: creates a new database, `SQLCipher`
    ///   encrypted when `key` is provided, plaintext otherwise.
    /// - Plain `SQLite`: opens plaintext; providing a key is an error
    ///   ([`SQLiteError::NotEncrypted`]) rather than a silent no-op so
    ///   callers never believe an unencrypted database is protected.
    /// - Compressed container: opens with the codec recorded in the
    ///   container header; the encryption flag decides whether `key`
    ///   is required ([`SQLiteError::EncryptionKeyRequired`]) or
    ///   rejected ([`SQLiteError::NotEncrypted`]).
    /// - Unrecognized header: treated as `SQLCipher` when `key` is
    ///   provided; without a key this fails with
    ///   [`SQLiteError::EncryptionKeyRequired`] because an encrypted
    ///   database cannot be told apart from a foreign file.
    ///
    /// New compressed containers are not created through this entry
    /// point; use [`Engine::open_compressed`] or
    /// [`Engine::open_compressed_encrypted`] to choose compression for
    /// a new database.
    pub fn open_auto(path: &Path, key: Option<&str>) -> Result<Self, SQLiteError> {
        use uqa_storage::DatabaseFileFormat;
        let key = match key {
            Some("") => return Err(SQLiteError::EmptyEncryptionKey),
            other => other,
        };
        match uqa_storage::detect_database_file_format(path)? {
            DatabaseFileFormat::Missing => match key {
                Some(key) => Self::open_encrypted(path, key),
                None => Self::open(path),
            },
            DatabaseFileFormat::PlainSQLite => match key {
                Some(_) => Err(SQLiteError::NotEncrypted),
                None => Self::open(path),
            },
            DatabaseFileFormat::CompressedContainer { encrypted: true } => match key {
                Some(key) => {
                    Self::open_compressed_encrypted(path, key, SQLiteCompressionOptions::default())
                }
                None => Err(SQLiteError::EncryptionKeyRequired),
            },
            DatabaseFileFormat::CompressedContainer { encrypted: false } => match key {
                Some(_) => Err(SQLiteError::NotEncrypted),
                None => Self::open_compressed(path, SQLiteCompressionOptions::default()),
            },
            DatabaseFileFormat::Unrecognized => match key {
                Some(key) => Self::open_encrypted(path, key),
                None => Err(SQLiteError::EncryptionKeyRequired),
            },
        }
    }

    /// SQLCipher-backed engine. Applies `key` before any catalog
    /// access, runs migrations, and rebuilds the in-memory table
    /// registry from the encrypted catalog.
    pub fn open_encrypted(path: &Path, key: &str) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open_encrypted(path, key)?;
        Self::open_with_connection(&conn)
    }

    /// Compressed SQLite-backed engine. The compression VFS is
    /// schema-neutral: it compresses `SQLite` byte ranges in chunks
    /// without knowledge of UQA catalog tables or columns.
    pub fn open_compressed(
        path: &Path,
        compression: SQLiteCompressionOptions,
    ) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open_compressed(path, compression)?;
        Self::open_with_connection(&conn)
    }

    /// Compressed and encrypted SQLite-backed engine. Chunk payloads
    /// are compressed first, then encrypted by the compressed VFS. The v2
    /// format authenticates container metadata, chunk placement, and commit
    /// records, but cannot distinguish replacement by an internally valid
    /// snapshot or fork without an external trusted state anchor.
    /// Security-sensitive deployments that do not require compression should
    /// prefer [`Engine::open_encrypted`] and `SQLCipher`.
    pub fn open_compressed_encrypted(
        path: &Path,
        key: &str,
        compression: SQLiteCompressionOptions,
    ) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open_compressed_encrypted(path, key, compression)?;
        Self::open_with_connection(&conn)
    }

    /// Open an encrypted compressed database and reject a different file or
    /// any state other than `trusted_anchor` before `SQLite` reads the main
    /// database. Refresh the trusted anchor after every committed write.
    pub fn open_compressed_encrypted_with_anchor(
        path: &Path,
        key: &str,
        compression: SQLiteCompressionOptions,
        trusted_anchor: SQLiteCompressedContainerAnchor,
    ) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open_compressed_encrypted_with_anchor(
            path,
            key,
            compression,
            trusted_anchor,
        )?;
        Self::open_with_connection(&conn)
    }

    /// Authenticate and return the anchor to persist in a trusted store after
    /// committed writes to an encrypted compressed database.
    pub fn compressed_container_anchor(
        path: &Path,
        key: &str,
    ) -> Result<SQLiteCompressedContainerAnchor, SQLiteError> {
        Ok(uqa_storage::read_authenticated_anchor(path, key)?)
    }

    fn open_with_connection(conn: &ManagedConnection) -> Result<Self, SQLiteError> {
        let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(conn.clone())?);
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(SQLiteStorageBackend::new(conn.clone()));
        let mut engine =
            Self::from_persistent_backends(catalog, backend).map_err(Self::sqlite_open_error)?;
        // Initial catalog migrations and physical-index repairs above may
        // commit through another pooled connection. Establish the monitor
        // baseline only after all one-time writes have completed.
        let data_version = conn.data_version()?.unwrap_or(0);
        engine.storage.sqlite_session = Some(conn.clone());
        engine
            .epochs
            .seen_sqlite_data_version
            .store(data_version, std::sync::atomic::Ordering::Release);
        Ok(engine)
    }

    /// Create an independent SQL session over this engine's `SQLite` database.
    ///
    /// The new session gets its own catalog/backend pair, transaction stack,
    /// runtime variables, prepared statements, statement cache, and
    /// cancellation token. Durable registry caches remain session-private and
    /// synchronize through shared epochs; runtime-only Rust extensions are
    /// shared. Table storage handles are rebound to the new
    /// [`ManagedConnection`] so all catalog/document/index/vector operations
    /// in an explicit transaction use one pinned physical connection.
    pub fn new_session(&self) -> Result<Self, SQLiteError> {
        let base = self.storage.sqlite_session.as_ref().ok_or_else(|| {
            SQLiteError::StorageBackend(
                "independent sessions require an Engine opened through the SQLite API".into(),
            )
        })?;
        let connection = base.new_session();
        let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(connection.clone())?);
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(SQLiteStorageBackend::new(connection.clone()));
        let mut session =
            Self::from_persistent_backends(catalog, backend).map_err(Self::sqlite_open_error)?;
        let data_version = connection.data_version()?.unwrap_or(0);

        session.storage.sqlite_session = Some(connection);
        session
            .epochs
            .seen_sqlite_data_version
            .store(data_version, std::sync::atomic::Ordering::Release);
        session.epochs.share_published_from(&self.epochs);
        // Force one catalog rebind after attaching the shared generation.
        // Otherwise a DDL commit racing the initial restore could leave this
        // session with the old table snapshot but the new generation marked
        // as already observed.
        // Durable registries remain session-local. Sharing these maps
        // would expose a writer's uncommitted graph/schema/view/FDW changes
        // to sibling sessions before SQLite COMMIT. Runtime-only registries
        // may remain shared.
        session.extensions = super::RuntimeExtensions::shared_from(&self.extensions);
        session
            .synchronize_table_catalog()
            .map_err(Self::sqlite_open_error)?;
        session
            .synchronize_table_data()
            .map_err(Self::sqlite_open_error)?;
        session
            .synchronize_catalog_registries()
            .map_err(Self::sqlite_open_error)?;
        Ok(session)
    }

    /// Build an engine from already-open persistent metadata and data
    /// backends. This is the storage-neutral entry point used by
    /// `Engine::open` after it creates the `SQLite` implementations,
    /// and by future `RocksDB` / `redb` constructors once they provide
    /// the same facade objects.
    pub fn from_persistent_backends(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
    ) -> StorageBackendResult<Self> {
        let restore_catalog = Arc::clone(&catalog);
        let restore_backend = Arc::clone(&backend);
        let mut engine = Self {
            storage: super::StorageContext::persistent(catalog, backend),
            durable: super::DurableCatalogState::new(),
            session: super::SessionContext::new(super::initial_random_state()),
            extensions: super::RuntimeExtensions::new(),
            epochs: super::EpochCoordinator::new(),
            runtime: super::QueryRuntime::new(super::SQL_FUNCTION_DEPTH_LIMIT),
        };
        Self::prepare_catalog_for_initial_restore(restore_catalog.as_ref())?;
        engine.restore_from_catalog(restore_catalog.as_ref(), restore_backend.as_ref())?;
        engine.repair_reset_fts_storage(restore_catalog.as_ref())?;
        engine.repair_persistent_value_indexes_on_open()?;
        // Eagerly and fallibly populate read caches. Once open succeeds,
        // cache misses mean absence rather than a swallowed catalog error.
        for (name, json) in restore_catalog.load_models()? {
            let model = serde_json::from_str::<DeepModel>(&json)?;
            engine.durable.models.write().insert(name, model);
        }
        for (name, json) in restore_catalog.load_all_scoring_params()? {
            engine.durable.scoring_params.write().insert(name, json);
        }
        Ok(engine)
    }

    fn sqlite_open_error(err: StorageBackendError) -> SQLiteError {
        match err {
            StorageBackendError::Analysis(err) => SQLiteError::Analysis(err),
            StorageBackendError::SQLite(err) => err,
            StorageBackendError::Serde(err) => SQLiteError::Serde(err),
            StorageBackendError::Other(msg) => SQLiteError::StorageBackend(msg),
        }
    }
}
