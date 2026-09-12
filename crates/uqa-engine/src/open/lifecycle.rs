//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! File-format opening, connection binding, and independent session creation.

use super::{
    Arc, DeepModel, Engine, ManagedConnection, Path, PersistentStorageBackend,
    PersistentStorageProvider, PersistentStorageSession, SQLiteCompressedContainerAnchor,
    SQLiteCompressionOptions, SQLiteError, SQLiteStorageProvider, StorageBackendError,
    StorageBackendResult,
};

struct BackendSessionProvider {
    backend: Arc<dyn PersistentStorageBackend>,
}

impl PersistentStorageProvider for BackendSessionProvider {
    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        self.backend.open_session()
    }

    fn storage_identity(
        &self,
    ) -> StorageBackendResult<Option<uqa_storage::PersistentStorageIdentity>> {
        self.backend.storage_identity()
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// In-memory engine. State lives only as long as this `Engine`.
    pub fn new() -> Self {
        let row_locks = Arc::new(crate::row_locks::RowLockManager::new());
        let notification_hub = Arc::new(crate::NotificationHub::default());
        let session_id = row_locks.allocate_session();
        Self {
            storage: super::StorageContext::memory(),
            durable: Arc::new(super::DurableCatalogState::new()),
            session: Arc::new(super::SessionContext::new(super::initial_random_state())),
            extensions: super::RuntimeExtensions::new(),
            epochs: super::EpochCoordinator::new(),
            runtime: super::QueryRuntime::new(super::SQL_FUNCTION_DEPTH_LIMIT),
            statistics: crate::statistics::shared_statistics(&row_locks),
            row_locks,
            notification_hub,
            session_id,
            owns_session_registration: true,
            query_table_snapshots: None,
            query_view_snapshots: None,
            query_sql_function_snapshots: None,
            query_catalog_snapshot: None,
            query_transaction_overlay: None,
            query_transaction_origin: None,
        }
    }

    pub fn open(path: &Path) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open(path)?;
        Self::open_with_connection(&conn)
    }

    /// Classify the on-disk format of `path` without opening it: plain
    /// `SQLite`, UQA compressed container (with its encryption flag), a
    /// missing/empty file, or an unrecognized header (`SQLCipher`
    /// databases fall here because `SQLCipher` encrypts the whole file).
    pub fn detect_database_file(
        path: &Path,
    ) -> std::io::Result<uqa_storage_sqlite::DatabaseFileFormat> {
        uqa_storage_sqlite::detect_database_file_format(path)
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
        use uqa_storage_sqlite::DatabaseFileFormat;
        let key = match key {
            Some("") => return Err(SQLiteError::EmptyEncryptionKey),
            other => other,
        };
        match uqa_storage_sqlite::detect_database_file_format(path)? {
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
        Ok(uqa_storage_sqlite::read_authenticated_anchor(path, key)?)
    }

    fn open_with_connection(conn: &ManagedConnection) -> Result<Self, SQLiteError> {
        let provider: Arc<dyn PersistentStorageProvider> =
            Arc::new(SQLiteStorageProvider::new(conn.clone()));
        Self::from_persistent_provider(provider).map_err(Self::sqlite_open_error)
    }

    /// Create an independent SQL session over this engine's durable database.
    ///
    /// The new session gets its own catalog/backend pair, transaction stack,
    /// runtime variables, prepared statements, statement cache, and
    /// cancellation token. Committed catalog definitions share immutable
    /// allocations; mutations detach the session's copy and synchronize through
    /// shared epochs. Runtime-only Rust extensions are shared. The provider
    /// must return catalog and data handles bound to one session transaction
    /// so every durable mutation commits atomically.
    pub fn new_session(&self) -> StorageBackendResult<Self> {
        let _statement = self.runtime.statement_gate.lock();
        let provider = self.storage.provider.as_ref().ok_or_else(|| {
            StorageBackendError::Other(
                "independent sessions require a PersistentStorageProvider".into(),
            )
        })?;
        // Fixed-snapshot construction can call this while holding the
        // transaction stack. In that case restore committed storage instead
        // of recursively locking the stack or sharing private definitions.
        let share_catalog = self
            .session
            .transactions
            .try_lock()
            .is_some_and(|stack| stack.is_empty())
            && !self.session.state.read().temporary_namespace_allocated;
        if share_catalog {
            self.synchronize_table_catalog()?;
            self.synchronize_table_data()?;
            self.synchronize_catalog_registries()?;
        }
        let observed_epochs = self.epochs.published_epochs();
        let storage_session = provider.open_session()?;
        let storage_version_before_restore = storage_session.backend.change_version()?;
        let shared = if share_catalog {
            self.session_from_shared_catalog(&storage_session, provider)?
        } else {
            None
        };
        let mut session = match shared {
            Some(session) => session,
            None => Self::from_initialized_persistent_session(
                storage_session,
                Some(Arc::clone(provider)),
            )?,
        };
        session.row_locks = Arc::clone(&self.row_locks);
        session.statistics = Arc::clone(&self.statistics);
        session.install_notification_hub(Arc::clone(&self.notification_hub))?;
        session.session_id = self.row_locks.allocate_session();
        let storage_version_after_restore = session
            .storage
            .backend
            .as_ref()
            .ok_or_else(|| {
                StorageBackendError::Other(
                    "persistent session lost its storage backend during restore".into(),
                )
            })?
            .change_version()?;
        if storage_version_before_restore == storage_version_after_restore {
            session
                .epochs
                .share_published_from_at(&self.epochs, observed_epochs);
        } else {
            session.epochs.share_published_from(&self.epochs);
        }
        // A commit that raced the load advanced a shared publication beyond
        // the generation or storage version captured before the session
        // restored. Only that case needs a second catalog or data refresh.
        // Catalog cells keep independent mutable owners over shared immutable
        // values, so a writer cannot expose uncommitted definitions to siblings.
        session.extensions = super::RuntimeExtensions::shared_from(&self.extensions);
        session.synchronize_table_catalog()?;
        session.synchronize_table_data()?;
        session.synchronize_catalog_registries()?;
        session.start_automatic_statistics();
        Ok(session)
    }

    /// Build an engine and retain the provider used to create future
    /// independent sessions.
    #[allow(clippy::needless_pass_by_value)]
    pub fn from_persistent_provider(
        provider: Arc<dyn PersistentStorageProvider>,
    ) -> StorageBackendResult<Self> {
        let identity = provider.storage_identity()?;
        let session = provider.open_session()?;
        let mut engine = Self::from_persistent_session(session, Some(Arc::clone(&provider)))?;
        let row_locks = crate::row_locks::shared_provider_manager(identity.clone(), &provider);
        let notification_hub =
            crate::notifications::shared_provider_notification_hub(identity, &provider);
        engine.session_id = row_locks.allocate_session();
        engine.statistics = crate::statistics::shared_statistics(&row_locks);
        engine.row_locks = row_locks;
        engine.install_notification_hub(notification_hub)?;
        engine.start_automatic_statistics();
        Ok(engine)
    }

    /// Build an engine from already-open persistent metadata and data backends. The backend's session factory is retained for independent SQL sessions and latest-committed row-lock rechecks. Prefer [`Self::from_persistent_provider`] when a database-level owner is already available.
    pub fn from_persistent_backends(
        catalog: Arc<dyn uqa_storage::CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
    ) -> StorageBackendResult<Self> {
        let identity = backend.storage_identity()?;
        let row_locks = crate::row_locks::shared_backend_manager(identity.clone(), &backend);
        let notification_hub =
            crate::notifications::shared_backend_notification_hub(identity, &backend);
        let provider: Arc<dyn PersistentStorageProvider> = Arc::new(BackendSessionProvider {
            backend: Arc::clone(&backend),
        });
        let mut engine = Self::from_persistent_session(
            PersistentStorageSession::new(catalog, backend),
            Some(provider),
        )?;
        engine.session_id = row_locks.allocate_session();
        engine.statistics = crate::statistics::shared_statistics(&row_locks);
        engine.row_locks = row_locks;
        engine.install_notification_hub(notification_hub)?;
        engine.start_automatic_statistics();
        Ok(engine)
    }

    fn install_notification_hub(
        &mut self,
        notification_hub: Arc<crate::NotificationHub>,
    ) -> StorageBackendResult<()> {
        let process_id = notification_hub
            .allocate_backend_process_id()
            .map_err(|error| {
                StorageBackendError::Other(format!(
                    "allocate database backend process identifier: {error}"
                ))
            })?;
        self.notification_hub = notification_hub;
        if let Some(process_id) = process_id {
            self.session.install_database_backend_process_id(process_id);
        }
        Ok(())
    }

    fn from_persistent_session(
        storage_session: PersistentStorageSession,
        provider: Option<Arc<dyn PersistentStorageProvider>>,
    ) -> StorageBackendResult<Self> {
        Self::build_persistent_session(storage_session, provider, true)
    }

    pub(crate) fn from_initialized_persistent_session(
        storage_session: PersistentStorageSession,
        provider: Option<Arc<dyn PersistentStorageProvider>>,
    ) -> StorageBackendResult<Self> {
        Self::build_persistent_session(storage_session, provider, false)
    }

    pub(super) fn empty_persistent_session(
        storage_session: PersistentStorageSession,
        provider: Option<Arc<dyn PersistentStorageProvider>>,
    ) -> Self {
        let PersistentStorageSession { catalog, backend } = storage_session;
        let row_locks = Arc::new(crate::row_locks::RowLockManager::new());
        let notification_hub = Arc::new(crate::NotificationHub::default());
        let session_id = row_locks.allocate_session();
        Self {
            storage: super::StorageContext::persistent(catalog, backend, provider),
            durable: Arc::new(super::DurableCatalogState::new()),
            session: Arc::new(super::SessionContext::new(super::initial_random_state())),
            extensions: super::RuntimeExtensions::new(),
            epochs: super::EpochCoordinator::new(),
            runtime: super::QueryRuntime::new(super::SQL_FUNCTION_DEPTH_LIMIT),
            statistics: crate::statistics::shared_statistics(&row_locks),
            row_locks,
            notification_hub,
            session_id,
            owns_session_registration: true,
            query_table_snapshots: None,
            query_view_snapshots: None,
            query_sql_function_snapshots: None,
            query_catalog_snapshot: None,
            query_transaction_overlay: None,
            query_transaction_origin: None,
        }
    }

    fn build_persistent_session(
        storage_session: PersistentStorageSession,
        provider: Option<Arc<dyn PersistentStorageProvider>>,
        initialize_catalog: bool,
    ) -> StorageBackendResult<Self> {
        let restore_catalog = Arc::clone(&storage_session.catalog);
        let restore_backend = Arc::clone(&storage_session.backend);
        let cache_revisions_before = restore_catalog.cache_revisions()?;
        let mut engine = Self::empty_persistent_session(storage_session, provider);
        if initialize_catalog {
            restore_backend.migrate_document_storage()?;
            // A clean restore remains read-only on backends that can promote a transaction, while backends without promotion reserve their writer before the atomic migration scan.
            restore_backend.begin_upgradeable_transaction()?;
            let restore_result = (|| {
                restore_backend.migrate_inverted_index_storage()?;
                Self::prepare_catalog_for_initial_restore(restore_catalog.as_ref())?;
                engine.restore_from_catalog(
                    restore_catalog.as_ref(),
                    restore_backend.as_ref(),
                    super::CatalogRestoreMode::InitialMigration,
                )
            })();
            if let Err(error) = restore_result {
                return match restore_backend.rollback_transaction() {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(StorageBackendError::Other(format!(
                        "rollback initial catalog migration after `{error}` failed: {rollback_error}"
                    ))),
                };
            }
            if let Err(error) = restore_backend.commit_transaction() {
                return match restore_backend.rollback_transaction() {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(StorageBackendError::Other(format!(
                        "rollback failed initial catalog migration commit `{error}`: {rollback_error}"
                    ))),
                };
            }
        } else {
            engine.restore_from_catalog(
                restore_catalog.as_ref(),
                restore_backend.as_ref(),
                super::CatalogRestoreMode::LoadOnly,
            )?;
        }
        if initialize_catalog {
            engine.repair_reset_fts_storage(restore_catalog.as_ref())?;
            engine.repair_persistent_value_indexes_on_open()?;
        }
        // Eagerly and fallibly populate read caches. Once open succeeds,
        // cache misses mean absence rather than a swallowed catalog error.
        for (name, json) in restore_catalog.load_models()? {
            let model = serde_json::from_str::<DeepModel>(&json)?;
            engine.durable.models.write().insert(name, model);
        }
        for (name, json) in restore_catalog.load_all_scoring_params()? {
            engine.durable.scoring_params.write().insert(name, json);
        }
        // Initial catalog migrations and physical-index repairs above may
        // commit. Establish the backend monitor baseline only after every
        // one-time write has completed.
        let cache_revisions_after = restore_catalog.cache_revisions()?;
        let stable_restore = cache_revisions_before == cache_revisions_after;
        if stable_restore {
            *engine.epochs.storage_cache_revisions.lock() = cache_revisions_after;
        }
        if let Some(version) = restore_backend.change_version()?.filter(|_| stable_restore) {
            engine
                .epochs
                .seen_storage_change_version
                .store(version, std::sync::atomic::Ordering::Release);
        }
        Ok(engine)
    }

    fn sqlite_open_error(err: StorageBackendError) -> SQLiteError {
        SQLiteError::from(err)
    }
}
