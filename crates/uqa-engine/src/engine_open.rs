//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    normalize_analyzer_phase, Analyzer, Arc, AtomicBool, BTreeMap, Catalog, CatalogFacade,
    ColumnStatsRow, DeepModel, Engine, FieldName, IVFIndexParams, ManagedConnection, Path,
    PersistentStorageBackend, RwLock, SQLiteCompressionOptions, SQLiteError, SQLiteStorageBackend,
    StorageBackendError, StorageBackendResult, TableSchema, TableState, Value, VectorIndex,
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
    /// are compressed first, then encrypted by the compressed VFS.
    pub fn open_compressed_encrypted(
        path: &Path,
        key: &str,
        compression: SQLiteCompressionOptions,
    ) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open_compressed_encrypted(path, key, compression)?;
        Self::open_with_connection(&conn)
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
        engine.sqlite_session = Some(conn.clone());
        engine
            .seen_sqlite_data_version
            .store(data_version, std::sync::atomic::Ordering::Release);
        Ok(engine)
    }

    /// Create an independent SQL session over this engine's `SQLite` database.
    ///
    /// The new session gets its own catalog/backend pair, transaction stack,
    /// runtime variables, prepared statements, statement cache, and
    /// cancellation token. Durable logical registries are shared, while table
    /// storage handles are rebound to the new [`ManagedConnection`] session so
    /// all catalog/document/index/vector operations in an explicit
    /// transaction use one pinned physical connection.
    pub fn new_session(&self) -> Result<Self, SQLiteError> {
        let base = self.sqlite_session.as_ref().ok_or_else(|| {
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

        session.sqlite_session = Some(connection);
        session
            .seen_sqlite_data_version
            .store(data_version, std::sync::atomic::Ordering::Release);
        session.table_catalog_epoch = self.table_catalog_epoch.clone();
        session.table_data_epoch = self.table_data_epoch.clone();
        session.catalog_registry_epoch = self.catalog_registry_epoch.clone();
        // Force one catalog rebind after attaching the shared generation.
        // Otherwise a DDL commit racing the initial restore could leave this
        // session with the old table snapshot but the new generation marked
        // as already observed.
        session
            .seen_table_catalog_epoch
            .store(0, std::sync::atomic::Ordering::Release);
        session
            .seen_table_data_epoch
            .store(0, std::sync::atomic::Ordering::Release);
        session
            .seen_catalog_registry_epoch
            .store(0, std::sync::atomic::Ordering::Release);
        // Durable registries remain session-local. Sharing these Arc maps
        // would expose a writer's uncommitted graph/schema/view/FDW changes
        // to sibling sessions before SQLite COMMIT. Runtime-only registries
        // may remain shared.
        session.foreign_memory_tables = self.foreign_memory_tables.clone();
        session.sql_scalar_functions = self.sql_scalar_functions.clone();
        session.sql_table_functions = self.sql_table_functions.clone();
        session.sql_aggregate_functions = self.sql_aggregate_functions.clone();
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
            statement_gate: parking_lot::ReentrantMutex::new(()),
            tables: RwLock::new(BTreeMap::new()),
            catalog: Some(catalog),
            backend: Some(backend),
            sqlite_session: None,
            seen_sqlite_data_version: std::sync::atomic::AtomicU64::new(0),
            external_commit_refresh: parking_lot::Mutex::new(()),
            table_catalog_epoch: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            seen_table_catalog_epoch: std::sync::atomic::AtomicU64::new(1),
            table_catalog_dirty: AtomicBool::new(false),
            table_catalog_refresh: parking_lot::Mutex::new(()),
            table_data_epoch: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            seen_table_data_epoch: std::sync::atomic::AtomicU64::new(1),
            table_data_dirty: AtomicBool::new(false),
            table_data_refresh: parking_lot::Mutex::new(()),
            catalog_registry_epoch: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            seen_catalog_registry_epoch: std::sync::atomic::AtomicU64::new(1),
            catalog_registry_dirty: AtomicBool::new(false),
            catalog_registry_refresh: parking_lot::Mutex::new(()),
            graphs: Arc::new(RwLock::new(BTreeMap::new())),
            models: RwLock::new(BTreeMap::new()),
            scoring_params: RwLock::new(BTreeMap::new()),
            views: Arc::new(RwLock::new(BTreeMap::new())),
            catalog_indexes: Arc::new(RwLock::new(BTreeMap::new())),
            schemas: Arc::new(RwLock::new(std::collections::BTreeSet::from([
                "public".to_string()
            ]))),
            search_path: RwLock::new(vec!["public".to_string()]),
            session_vars: RwLock::new(BTreeMap::new()),
            random_state: parking_lot::Mutex::new(super::initial_random_state()),
            path_indexes: Arc::new(RwLock::new(BTreeMap::new())),
            tx_stack: parking_lot::Mutex::new(Vec::new()),
            cancel: uqa_core::CancellationToken::new(),
            sequences: RwLock::new(BTreeMap::new()),
            sequence_currvals: RwLock::new(BTreeMap::new()),
            prepared: RwLock::new(BTreeMap::new()),
            sql_statement_cache: RwLock::new(super::SQLStatementCache::default()),
            named_analyzers: Arc::new(RwLock::new(BTreeMap::new())),
            table_field_analyzers: Arc::new(RwLock::new(BTreeMap::new())),
            foreign_servers: Arc::new(RwLock::new(BTreeMap::new())),
            foreign_tables: Arc::new(RwLock::new(BTreeMap::new())),
            foreign_memory_tables: Arc::new(RwLock::new(BTreeMap::new())),
            sql_scalar_functions: Arc::new(RwLock::new(BTreeMap::new())),
            sql_table_functions: Arc::new(RwLock::new(BTreeMap::new())),
            sql_aggregate_functions: Arc::new(RwLock::new(BTreeMap::new())),
            sql_user_functions: Arc::new(RwLock::new(BTreeMap::new())),
            sql_notices: parking_lot::Mutex::new(Vec::new()),
            sql_function_depth_limit: std::sync::atomic::AtomicUsize::new(
                super::SQL_FUNCTION_DEPTH_LIMIT,
            ),
        };
        Self::prepare_catalog_for_initial_restore(restore_catalog.as_ref())?;
        engine.restore_from_catalog(restore_catalog.as_ref(), restore_backend.as_ref())?;
        engine.repair_reset_fts_storage(restore_catalog.as_ref())?;
        engine.repair_persistent_value_indexes_on_open()?;
        // Eagerly and fallibly populate read caches. Once open succeeds,
        // cache misses mean absence rather than a swallowed catalog error.
        for (name, json) in restore_catalog.load_models()? {
            let model = serde_json::from_str::<DeepModel>(&json)?;
            engine.models.write().insert(name, model);
        }
        for (name, json) in restore_catalog.load_all_scoring_params()? {
            engine.scoring_params.write().insert(name, json);
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

    fn restore_from_catalog(
        &mut self,
        catalog: &dyn CatalogFacade,
        backend: &dyn PersistentStorageBackend,
    ) -> StorageBackendResult<()> {
        self.restore_schemas_from_catalog(catalog)?;
        let schemas = catalog.load_tables()?;
        for schema in schemas {
            let relation = schema.relation.clone();
            let table = Self::load_session_table(catalog, backend, schema)?;
            self.tables.write().insert(relation, table);
        }
        self.restore_graphs_from_catalog(catalog)?;
        self.restore_engine_registries_from_catalog(catalog)?;
        Ok(())
    }

    /// Perform catalog mutations that are permitted only while opening a new
    /// engine session. Every later snapshot/reload path is deliberately
    /// load-only so a read transaction or rollback cannot commit a repair.
    fn prepare_catalog_for_initial_restore(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        catalog.migrate_relation_namespace()?;
        let schemas = catalog.load_schemas()?;
        for schema in &schemas {
            Self::validate_schema_name(schema)?;
        }
        if !schemas.iter().any(|name| name == "public") {
            catalog.save_schema("public")?;
        }
        Self::migrate_legacy_sequences_from_metadata(catalog)
    }

    fn load_session_table(
        catalog: &dyn CatalogFacade,
        backend: &dyn PersistentStorageBackend,
        schema: TableSchema,
    ) -> StorageBackendResult<Arc<TableState>> {
        let table_name = schema.relation.qualified_name();
        let analyzer: Analyzer = serde_json::from_str(&schema.analyzer_json)?;
        let docs = backend.document_store(&table_name);
        let inv = backend.inverted_index(&table_name, analyzer.clone());
        let mut vectors: BTreeMap<FieldName, Box<dyn VectorIndex>> = BTreeMap::new();
        for vector_field in &schema.vector_fields {
            vectors.insert(
                vector_field.field.clone(),
                backend.vector_index(
                    &table_name,
                    &vector_field.field,
                    vector_field.dimensions,
                    None,
                ),
            );
        }
        let columns: Vec<uqa_sql::ast::ColumnDef> = if schema.columns_json.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&schema.columns_json)?
        };
        let constraints: uqa_sql::ast::TableConstraintSet = if schema.constraints_json.is_empty() {
            uqa_sql::ast::TableConstraintSet::default()
        } else {
            serde_json::from_str(&schema.constraints_json)?
        };
        let column_stats = Self::load_column_stats_from_catalog(catalog, &table_name)?;
        let column_stats_dirty = column_stats.is_empty() && !columns.is_empty();
        let max_id = docs.max_doc_id()?;
        Ok(Arc::new(TableState {
            document_store: RwLock::new(docs),
            inverted_index: RwLock::new(inv),
            vector_indexes: RwLock::new(vectors),
            fts_fields: RwLock::new(schema.fts_fields),
            columns: RwLock::new(columns),
            next_id: parking_lot::Mutex::new(u128::from(max_id) + 1),
            analyzer: RwLock::new(analyzer),
            column_stats: RwLock::new(column_stats),
            column_stats_loaded: AtomicBool::new(true),
            column_stats_dirty: AtomicBool::new(column_stats_dirty),
            table_checks: RwLock::new(constraints.checks),
            foreign_keys: RwLock::new(constraints.foreign_keys),
            key_constraints: RwLock::new(constraints.key_constraints),
            value_indexes: RwLock::new(BTreeMap::new()),
            doc_count_cache: std::sync::atomic::AtomicU64::new(0),
            doc_count_dirty: AtomicBool::new(true),
        }))
    }

    /// Publish a committed logical table-definition change to sibling
    /// sessions. Their physical stores are rebuilt lazily from their own
    /// session-bound backend on the next table lookup.
    pub(crate) fn note_table_catalog_changed(&self) {
        if !self.tx_stack.lock().is_empty() {
            self.table_catalog_dirty
                .store(true, std::sync::atomic::Ordering::Release);
            return;
        }
        self.publish_table_catalog_changes();
    }

    pub(crate) fn publish_table_catalog_changes(&self) {
        self.table_catalog_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.table_catalog_dirty
            .store(false, std::sync::atomic::Ordering::Release);
        // The writer's physical stores are current, but cached optimized and
        // prepared plans may retain a removed access path or old schema.
        // Leave `seen_table_catalog_epoch` behind so its next statement also
        // crosses the same reload/re-optimization boundary as siblings.
        self.clear_sql_statement_cache();
    }

    /// Mark table contents changed in this session. The generation is only
    /// published after the outer storage transaction commits, so sibling
    /// sessions cannot invalidate and rebuild against uncommitted data.
    pub(crate) fn note_table_data_changed(&self) {
        self.clear_sql_statement_cache();
        // Rollback restoration replaces snapshots directly and never enters
        // this ordinary mutation hook. Therefore contention is not evidence
        // of an active transaction: wait for the stack and inspect its state.
        // This prevents an unrelated session thread from turning an
        // autocommit write into an unpublished dirty generation.
        let transaction_active = !self.tx_stack.lock().is_empty();
        if transaction_active {
            self.table_data_dirty
                .store(true, std::sync::atomic::Ordering::Release);
            return;
        }
        self.publish_table_data_changes();
    }

    pub(crate) fn publish_table_data_changes(&self) {
        self.table_data_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        // Keep this session's observed generation behind too. Its ordinary
        // write caches were updated incrementally, but prepared/optimized
        // plans and every derived store must cross the same refresh boundary
        // as sibling sessions before the next statement.
        self.table_data_dirty
            .store(false, std::sync::atomic::Ordering::Release);
        self.clear_sql_statement_cache();
    }

    /// Refresh every session-local dependency of committed table contents.
    /// Calls made inside an already-pinned `SQLite` transaction intentionally
    /// defer the refresh: that transaction must keep using its original
    /// snapshot and will observe the new generation after it finishes.
    pub(crate) fn synchronize_table_data(&self) -> StorageBackendResult<()> {
        if self
            .sqlite_session
            .as_ref()
            .is_some_and(ManagedConnection::in_transaction)
        {
            return Ok(());
        }
        self.synchronize_external_commits()?;
        self.refresh_table_data_cache(false)
    }

    /// Detect commits made by independently opened engines or other
    /// processes. In-process Arc epochs only coordinate sessions derived via
    /// `new_session`; `SQLite`'s per-connection `data_version` closes the same
    /// visibility gap for every other writer.
    fn synchronize_external_commits(&self) -> StorageBackendResult<()> {
        let Some(connection) = self.sqlite_session.as_ref() else {
            return Ok(());
        };
        if connection.in_transaction() {
            return Ok(());
        }
        let Some(version) = connection.data_version()? else {
            return Ok(());
        };
        if self
            .seen_sqlite_data_version
            .load(std::sync::atomic::Ordering::Acquire)
            == version
        {
            return Ok(());
        }

        let _statement = self.statement_gate.lock();
        let _refresh = self.external_commit_refresh.lock();
        if connection.in_transaction() {
            return Ok(());
        }
        let Some(version) = connection.data_version()? else {
            return Ok(());
        };
        if self
            .seen_sqlite_data_version
            .load(std::sync::atomic::Ordering::Acquire)
            == version
        {
            return Ok(());
        }

        // Mark this version while rebuilding so catalog restore helpers that
        // resolve a table cannot recursively enter the same non-reentrant
        // refresh lock. Restore the old marker on failure; a commit racing the
        // rebuild will advance the monitor again and be handled next time.
        let previous_version = self
            .seen_sqlite_data_version
            .swap(version, std::sync::atomic::Ordering::AcqRel);
        let refresh_result = (|| {
            self.clear_persistent_table_bindings_for_catalog_reload();
            let table_catalog_epoch = self
                .table_catalog_epoch
                .load(std::sync::atomic::Ordering::Acquire);
            self.reload_table_catalog(table_catalog_epoch)?;
            self.refresh_table_data_cache(true)?;
            let catalog_registry_epoch = self
                .catalog_registry_epoch
                .load(std::sync::atomic::Ordering::Acquire);
            self.reload_catalog_registries(catalog_registry_epoch)
        })();
        if refresh_result.is_err() {
            self.seen_sqlite_data_version
                .store(previous_version, std::sync::atomic::Ordering::Release);
        }
        refresh_result
    }

    fn refresh_table_data_cache(&self, force: bool) -> StorageBackendResult<()> {
        let target_epoch = self
            .table_data_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        if !force
            && self
                .seen_table_data_epoch
                .load(std::sync::atomic::Ordering::Acquire)
                == target_epoch
        {
            return Ok(());
        }

        let _refresh = self.table_data_refresh.lock();
        let target_epoch = self
            .table_data_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let previous_epoch = self
            .seen_table_data_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        if !force && previous_epoch == target_epoch {
            return Ok(());
        }

        let tables = self
            .tables
            .read()
            .iter()
            .map(|(name, table)| (name.clone(), table.clone()))
            .collect::<Vec<_>>();
        for (name, table) in tables {
            let name = name.qualified_name();
            if self.backend.is_some() {
                self.rebind_persistent_table_stores(&name, &table)?;
                let next_id = u128::from(table.document_store.read().max_doc_id()?) + 1;
                let mut current = table.next_id.lock();
                *current = (*current).max(next_id);
            } else {
                Self::value_indexes_clear(&table);
            }
            table
                .doc_count_dirty
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(catalog) = self.catalog.as_ref() {
                let stats = Self::load_column_stats_from_catalog(catalog.as_ref(), &name)?;
                let stats_dirty = stats.is_empty() && !table.columns.read().is_empty();
                *table.column_stats.write() = stats;
                table
                    .column_stats_loaded
                    .store(true, std::sync::atomic::Ordering::Release);
                table
                    .column_stats_dirty
                    .store(stats_dirty, std::sync::atomic::Ordering::Release);
            } else {
                table
                    .column_stats_dirty
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        self.clear_sql_statement_cache();
        // Set the generation before rebinding prepared plans so optimizer
        // statistics can resolve tables without recursively refreshing.
        self.seen_table_data_epoch
            .store(target_epoch, std::sync::atomic::Ordering::Release);
        if let Err(error) = self.rebind_prepared_plans() {
            self.seen_table_data_epoch
                .store(previous_epoch, std::sync::atomic::Ordering::Release);
            return Err(StorageBackendError::Other(format!(
                "re-optimize prepared plans after table data refresh: {error}"
            )));
        }
        Ok(())
    }

    /// Bring every session-local cache onto the outer transaction's pinned
    /// database snapshot. A stable `data_version` closes the gap between a
    /// physical `SQLite` commit and publication of the matching in-process
    /// epochs, while allowing unchanged statements to retain their caches.
    pub(crate) fn refresh_pinned_transaction_snapshot(&self) -> StorageBackendResult<()> {
        let (sqlite_snapshot_unchanged, stable_sqlite_version) =
            if let Some(connection) = self.sqlite_session.as_ref() {
                if connection.data_version_monitor_is_nonblocking()? {
                    let before = connection.data_version()?;
                    connection.pin_transaction_snapshot()?;
                    let after = connection.data_version()?;
                    let stable = before == after;
                    (
                        stable
                            && after.is_some_and(|version| {
                                self.seen_sqlite_data_version
                                    .load(std::sync::atomic::Ordering::Acquire)
                                    == version
                            }),
                        stable.then_some(after).flatten(),
                    )
                } else {
                    // A compressed rollback-journal writer owns the VFS's
                    // whole-file exclusive lock. Pin and refresh through that
                    // connection; querying the independent monitor would wait on
                    // a lock held by this same session.
                    connection.pin_transaction_snapshot()?;
                    (false, None)
                }
            } else {
                (true, None)
            };
        let table_catalog_epoch = self
            .table_catalog_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let table_data_epoch = self
            .table_data_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let catalog_registry_epoch = self
            .catalog_registry_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        if sqlite_snapshot_unchanged
            && self
                .seen_table_catalog_epoch
                .load(std::sync::atomic::Ordering::Acquire)
                == table_catalog_epoch
            && self
                .seen_table_data_epoch
                .load(std::sync::atomic::Ordering::Acquire)
                == table_data_epoch
            && self
                .seen_catalog_registry_epoch
                .load(std::sync::atomic::Ordering::Acquire)
                == catalog_registry_epoch
        {
            return Ok(());
        }

        self.clear_persistent_table_bindings_for_catalog_reload();
        self.reload_table_catalog(table_catalog_epoch)?;
        self.refresh_table_data_cache(true)?;
        self.reload_catalog_registries(catalog_registry_epoch)?;
        if let Some(version) = stable_sqlite_version {
            self.seen_sqlite_data_version
                .store(version, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }

    /// Mark a durable non-table registry change. Explicit transactions keep
    /// the generation private until their outer COMMIT; autocommit operations
    /// publish immediately.
    pub(crate) fn note_catalog_registry_changed(&self) {
        if !self.tx_stack.lock().is_empty() {
            self.catalog_registry_dirty
                .store(true, std::sync::atomic::Ordering::Release);
            return;
        }
        self.publish_catalog_registry_changes();
    }

    pub(crate) fn publish_catalog_registry_changes(&self) {
        self.catalog_registry_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.catalog_registry_dirty
            .store(false, std::sync::atomic::Ordering::Release);
        self.clear_sql_statement_cache();
    }

    /// Rebind this session's physical table handles when another session has
    /// changed the durable table catalog. Logical definitions come from the
    /// catalog; document/FTS/vector handles always come from `self.backend`.
    pub(crate) fn synchronize_table_catalog(&self) -> StorageBackendResult<()> {
        // An explicit transaction owns a pinned SQLite snapshot. Never consume
        // a sibling's newer in-process epoch while reading that older
        // snapshot; the next call after COMMIT/ROLLBACK will perform the
        // refresh. Outer BEGIN uses `refresh_pinned_transaction_snapshot`
        // directly after acquiring its snapshot.
        if self
            .sqlite_session
            .as_ref()
            .is_some_and(ManagedConnection::in_transaction)
        {
            return Ok(());
        }
        self.synchronize_external_commits()?;
        let target_epoch = self
            .table_catalog_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        if self
            .seen_table_catalog_epoch
            .load(std::sync::atomic::Ordering::Acquire)
            == target_epoch
        {
            return Ok(());
        }

        let _refresh = self.table_catalog_refresh.lock();
        let target_epoch = self
            .table_catalog_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        if self
            .seen_table_catalog_epoch
            .load(std::sync::atomic::Ordering::Acquire)
            == target_epoch
        {
            return Ok(());
        }
        self.reload_table_catalog(target_epoch)
    }

    pub(crate) fn reload_table_catalog_after_rollback(&self) -> StorageBackendResult<()> {
        self.clear_persistent_table_bindings_for_catalog_reload();
        let target_epoch = self
            .table_catalog_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        self.reload_table_catalog(target_epoch)?;
        self.table_catalog_dirty
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// A composite catalog refresh reloads these maps from durable rows after
    /// rebuilding table handles. Clear them first so an uncommitted/rolled-
    /// back analyzer or vector-index binding cannot be applied to the fresh
    /// stores during the intermediate table reload.
    fn clear_persistent_table_bindings_for_catalog_reload(&self) {
        self.table_field_analyzers.write().clear();
        self.catalog_indexes.write().clear();
    }

    fn reload_table_catalog(&self, target_epoch: u64) -> StorageBackendResult<()> {
        let previous_epoch = self
            .seen_table_catalog_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let Some(catalog) = self.catalog.as_ref() else {
            self.seen_table_catalog_epoch
                .store(target_epoch, std::sync::atomic::Ordering::Release);
            return Ok(());
        };
        let Some(backend) = self.backend.as_ref() else {
            return Err(StorageBackendError::Other(
                "persistent catalog has no matching storage backend".into(),
            ));
        };

        let mut rebound = BTreeMap::new();
        for schema in catalog.load_tables()? {
            let relation = schema.relation.clone();
            rebound.insert(
                relation,
                Self::load_session_table(catalog.as_ref(), backend.as_ref(), schema)?,
            );
        }
        *self.tables.write() = rebound;

        // Restore per-field analyzers and IVF/HNSW bindings on the newly
        // created session-local stores. These registries are logical state
        // shared by sibling sessions.
        let tables = self
            .tables
            .read()
            .iter()
            .map(|(name, table)| (name.clone(), table.clone()))
            .collect::<Vec<_>>();
        for (name, table) in tables {
            self.rebind_persistent_table_stores(&name.qualified_name(), &table)?;
        }
        self.seen_table_catalog_epoch
            .store(target_epoch, std::sync::atomic::Ordering::Release);
        self.clear_sql_statement_cache();
        if let Err(error) = self.rebind_prepared_plans() {
            self.seen_table_catalog_epoch
                .store(previous_epoch, std::sync::atomic::Ordering::Release);
            return Err(StorageBackendError::Other(format!(
                "re-optimize prepared plans after table catalog refresh: {error}"
            )));
        }
        Ok(())
    }

    /// Refresh session-local durable registry caches after a sibling commits.
    /// The catalog connection supplies `SQLite` snapshot isolation, so a reader
    /// never observes another session's uncommitted registry changes.
    pub(crate) fn synchronize_catalog_registries(&self) -> StorageBackendResult<()> {
        if self
            .sqlite_session
            .as_ref()
            .is_some_and(ManagedConnection::in_transaction)
        {
            return Ok(());
        }
        self.synchronize_external_commits()?;
        let target_epoch = self
            .catalog_registry_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        if self
            .seen_catalog_registry_epoch
            .load(std::sync::atomic::Ordering::Acquire)
            == target_epoch
        {
            return Ok(());
        }
        let _refresh = self.catalog_registry_refresh.lock();
        let target_epoch = self
            .catalog_registry_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        if self
            .seen_catalog_registry_epoch
            .load(std::sync::atomic::Ordering::Acquire)
            == target_epoch
        {
            return Ok(());
        }
        self.reload_catalog_registries(target_epoch)
    }

    pub(crate) fn reload_catalog_registries_after_rollback(&self) -> StorageBackendResult<()> {
        let target_epoch = self
            .catalog_registry_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        self.reload_catalog_registries(target_epoch)?;
        self.catalog_registry_dirty
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    fn reload_catalog_registries(&self, target_epoch: u64) -> StorageBackendResult<()> {
        let previous_epoch = self
            .seen_catalog_registry_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let Some(catalog) = self.catalog.as_ref() else {
            self.seen_catalog_registry_epoch
                .store(target_epoch, std::sync::atomic::Ordering::Release);
            return Ok(());
        };

        self.graphs.write().clear();
        self.views.write().clear();
        self.catalog_indexes.write().clear();
        self.schemas.write().clear();
        self.path_indexes.write().clear();
        self.named_analyzers.write().clear();
        self.table_field_analyzers.write().clear();
        self.foreign_servers.write().clear();
        self.foreign_tables.write().clear();
        self.sql_user_functions.write().clear();
        self.models.write().clear();
        self.scoring_params.write().clear();

        self.restore_schemas_from_catalog(catalog.as_ref())?;
        self.restore_graphs_from_catalog(catalog.as_ref())?;
        self.restore_engine_registries_from_catalog(catalog.as_ref())?;
        for (name, json) in catalog.load_models()? {
            self.models
                .write()
                .insert(name, serde_json::from_str::<DeepModel>(&json)?);
        }
        for (name, json) in catalog.load_all_scoring_params()? {
            self.scoring_params.write().insert(name, json);
        }
        // Registry restoration can remove a table-field analyzer or replace a
        // vector/index binding. Recreate every persistent store after the
        // registry maps hold the durable snapshot; otherwise a rolled-back
        // analyzer that was applied during the preceding table reload can
        // survive in the physical session handle even though its catalog row
        // is gone.
        let tables = self
            .tables
            .read()
            .iter()
            .map(|(relation, table)| (relation.qualified_name(), table.clone()))
            .collect::<Vec<_>>();
        for (name, table) in tables {
            self.rebind_persistent_table_stores(&name, &table)?;
        }
        self.seen_catalog_registry_epoch
            .store(target_epoch, std::sync::atomic::Ordering::Release);
        self.clear_sql_statement_cache();
        if let Err(error) = self.rebind_prepared_plans() {
            self.seen_catalog_registry_epoch
                .store(previous_epoch, std::sync::atomic::Ordering::Release);
            return Err(StorageBackendError::Other(format!(
                "re-optimize prepared plans after catalog registry refresh: {error}"
            )));
        }
        Ok(())
    }

    fn restore_schemas_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let schemas = catalog.load_schemas()?;
        for schema in &schemas {
            Self::validate_schema_name(schema)?;
        }
        if !schemas.iter().any(|name| name == "public") {
            return Err(StorageBackendError::Other(
                "catalog is missing required schema `public`".to_string(),
            ));
        }
        *self.schemas.write() = schemas.into_iter().collect();
        Ok(())
    }

    pub(crate) fn load_column_stats_from_catalog(
        catalog: &dyn CatalogFacade,
        table_name: &str,
    ) -> StorageBackendResult<BTreeMap<String, uqa_planner::ColumnStats>> {
        let mut out = BTreeMap::new();
        for row in catalog.load_column_stats(table_name)? {
            out.insert(row.column_name.clone(), Self::column_stats_from_row(row)?);
        }
        Ok(out)
    }

    fn column_stats_from_row(
        row: ColumnStatsRow,
    ) -> StorageBackendResult<uqa_planner::ColumnStats> {
        Ok(uqa_planner::ColumnStats {
            distinct_count: row.distinct_count.try_into().map_err(|_| {
                StorageBackendError::Other(format!(
                    "negative distinct_count for column `{}`",
                    row.column_name
                ))
            })?,
            null_count: row.null_count.try_into().map_err(|_| {
                StorageBackendError::Other(format!(
                    "negative null_count for column `{}`",
                    row.column_name
                ))
            })?,
            min_value: Self::decode_column_stat_value(row.min_value)?,
            max_value: Self::decode_column_stat_value(row.max_value)?,
            row_count: row.row_count.try_into().map_err(|_| {
                StorageBackendError::Other(format!(
                    "negative row_count for column `{}`",
                    row.column_name
                ))
            })?,
            histogram: serde_json::from_str(&row.histogram_json)?,
            mcv_values: serde_json::from_str(&row.mcv_values_json)?,
            mcv_frequencies: serde_json::from_str(&row.mcv_frequencies_json)?,
        })
    }

    fn decode_column_stat_value(raw: Option<String>) -> StorageBackendResult<Option<Value>> {
        let Some(raw) = raw else {
            return Ok(None);
        };
        match serde_json::from_str::<Value>(&raw)? {
            Value::Null => Ok(None),
            value => Ok(Some(value)),
        }
    }

    /// Re-hydrate the named-analyzer / table-field-analyzer / foreign
    /// server / foreign table / catalog index / path index registries
    /// from the catalog. Mirrors the side effects of every
    /// `register_*` method but skips their catalog write-back so the
    /// load is idempotent.
    fn restore_engine_registries_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        self.restore_sequences_from_catalog(catalog)?;
        self.restore_sql_functions_from_metadata(catalog)?;
        self.restore_analyzers_from_catalog(catalog)?;
        self.restore_foreign_registries_from_catalog(catalog)?;
        // Stored view plans are rebound only after every row-producing
        // relation kind is present. Legacy unqualified sources may refer to a
        // foreign table and must not be classified as missing during reopen.
        self.restore_views_from_catalog(catalog)?;
        self.restore_catalog_indexes_from_catalog(catalog)?;
        self.restore_path_indexes_from_catalog(catalog)?;
        Ok(())
    }

    fn restore_analyzers_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (name, config_json) in catalog.load_analyzers()? {
            super::parse_analyzer_config(&name, &config_json)
                .map_err(StorageBackendError::Other)?;
            self.named_analyzers.write().insert(name, config_json);
        }
        for (table, field, phase, analyzer_name) in catalog.load_table_field_analyzers()? {
            let t = self.try_table(&table)?.ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "table-field analyzer references missing table `{table}`"
                ))
            })?;
            Self::validate_table_analyzer_field(&table, &t, &field)
                .map_err(StorageBackendError::Other)?;
            let analyzer = self
                .resolve_analyzer(&analyzer_name)
                .map_err(StorageBackendError::Other)?;
            let (phase_name, normalized_phase) =
                normalize_analyzer_phase(&phase).map_err(StorageBackendError::Other)?;
            t.inverted_index
                .write()
                .set_field_analyzer(&field, analyzer, normalized_phase)
                .map_err(StorageBackendError::Other)?;
            self.table_field_analyzers
                .write()
                .insert((table, field), (analyzer_name, phase_name));
        }
        Ok(())
    }

    fn restore_foreign_registries_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (name, fdw_type, options_json) in catalog.load_foreign_servers()? {
            let options: BTreeMap<String, String> = serde_json::from_str(&options_json)?;
            self.foreign_servers.write().insert(
                name.clone(),
                uqa_fdw::ForeignServer {
                    name,
                    fdw_type,
                    options,
                },
            );
        }
        for row in catalog.load_foreign_tables()? {
            let relation_name = row.relation.qualified_name();
            if !self.foreign_servers.read().contains_key(&row.server_name) {
                return Err(StorageBackendError::Other(format!(
                    "foreign table `{}` references missing server `{}`",
                    relation_name, row.server_name
                )));
            }
            let columns: Vec<uqa_sql::ast::ColumnDef> = serde_json::from_str(&row.columns_json)?;
            let options: BTreeMap<String, String> = serde_json::from_str(&row.options_json)?;
            let fdw_columns: Vec<uqa_fdw::ColumnDef> = columns
                .iter()
                .map(|c| uqa_fdw::ColumnDef {
                    name: c.name.clone(),
                    ty: crate::engine_fdw::sql_column_type_to_fdw(&c.ty),
                })
                .collect();
            self.foreign_tables.write().insert(
                row.relation,
                uqa_fdw::ForeignTable {
                    name: relation_name,
                    server_name: row.server_name,
                    columns: fdw_columns,
                    options,
                },
            );
        }
        Ok(())
    }

    fn restore_catalog_indexes_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for row in catalog.load_catalog_indexes()? {
            if !self.try_has_table(&row.table_name)? {
                return Err(StorageBackendError::Other(format!(
                    "catalog index `{}` references missing table `{}`",
                    row.name, row.table_name
                )));
            }
            self.catalog_indexes
                .write()
                .insert(row.name.clone(), row.clone());
            let columns: Vec<String> = serde_json::from_str(&row.columns_json)?;
            let parameters: BTreeMap<String, String> = serde_json::from_str(&row.parameters_json)?;
            if row.index_type.eq_ignore_ascii_case("gin") {
                let analyzer = parameters
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("analyzer"))
                    .map(|(_, v)| v.as_str());
                for col in &columns {
                    self.restore_fts_field_from_catalog(&row.table_name, col, analyzer)
                        .map_err(StorageBackendError::Other)?;
                }
            } else if row.index_type.eq_ignore_ascii_case("ivf")
                || row.index_type.eq_ignore_ascii_case("hnsw")
            {
                let params = IVFIndexParams::from_catalog_map(&parameters)?;
                for col in &columns {
                    let Some(
                        uqa_sql::ast::ColumnType::Vector(dim)
                        | uqa_sql::ast::ColumnType::Tensor(dim),
                    ) = self.column_type(&row.table_name, col)?
                    else {
                        return Err(StorageBackendError::Other(format!(
                            "vector index `{}` references missing or non-vector column `{}`.`{col}`",
                            row.name, row.table_name
                        )));
                    };
                    if !self.restore_ivf_vector_field(&row.table_name, col, dim, params)? {
                        return Err(StorageBackendError::Other(format!(
                            "failed to restore vector index `{}` for table `{}`",
                            row.name, row.table_name
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Rebuild FTS postings once after `Catalog::open` had to replace an
    /// incompatible legacy storage shape. The catalog's reset marker is tied
    /// to that open operation and intentionally must not be consulted by
    /// runtime registry reloads, where rebuilding would turn reads and
    /// rollback cleanup into writes.
    fn repair_reset_fts_storage(&self, catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
        if !catalog.fts_storage_was_reset() {
            return Ok(());
        }
        let tables = self
            .catalog_indexes
            .read()
            .values()
            .filter(|row| row.index_type.eq_ignore_ascii_case("gin"))
            .map(|row| row.table_name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for table_name in tables {
            let table = self.try_table(&table_name)?.ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "GIN catalog repair references missing table `{table_name}`"
                ))
            })?;
            Self::rebuild_fts_index(&table).map_err(StorageBackendError::Other)?;
        }
        Ok(())
    }

    fn restore_path_indexes_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (key, seq_json) in catalog.load_path_indexes()? {
            let label_sequences: Vec<Vec<String>> = serde_json::from_str(&seq_json)?;
            let (graph, name) = key.split_once("::").ok_or_else(|| {
                StorageBackendError::Other(format!("invalid path-index key `{key}`"))
            })?;
            if graph.is_empty() || name.is_empty() {
                return Err(StorageBackendError::Other(format!(
                    "invalid path-index key `{key}`"
                )));
            }
            let graphs = self.graphs.read();
            let store = graphs.get(graph).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "path index `{key}` references missing graph `{graph}`"
                ))
            })?;
            let idx = uqa_graph::PathIndex::build(store, graph, &label_sequences)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            drop(graphs);
            self.path_indexes.write().insert(key, idx);
        }
        Ok(())
    }

    fn restore_graphs_from_catalog(&self, catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;

        // Step 1: register every named graph (the registry table is
        // authoritative for empty graphs).
        let names = catalog.load_named_graphs()?;
        let mut graphs = self.graphs.write();
        for name in &names {
            graphs.entry(name.clone()).or_default();
            if let Some(store) = graphs.get_mut(name) {
                if !store.has_graph(name) {
                    store.create_graph(name);
                }
            }
        }

        // Step 2: load every entity into side tables. Memberships, rather
        // than the global entity rows, determine each graph partition.
        let (vertex_by_id, edge_by_id) = Self::load_graph_entities(catalog)?;
        let memberships = catalog.load_graph_memberships()?;
        Self::restore_graph_memberships(&mut graphs, &memberships, &vertex_by_id, &edge_by_id)?;
        Self::restore_graph_label_registries(&mut graphs, catalog)
    }

    fn load_graph_entities(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<(
        BTreeMap<u64, uqa_core::Vertex>,
        BTreeMap<u64, uqa_core::Edge>,
    )> {
        let vertex_rows = catalog.load_vertices()?;
        let mut vertex_by_id: BTreeMap<u64, uqa_core::Vertex> = BTreeMap::new();
        for (id, label, props_json) in vertex_rows {
            let properties: BTreeMap<String, uqa_core::Value> = serde_json::from_str(&props_json)?;
            vertex_by_id.insert(
                id,
                uqa_core::Vertex {
                    vertex_id: id,
                    label,
                    properties,
                },
            );
        }
        let edge_rows = catalog.load_edges()?;
        let mut edge_by_id: BTreeMap<u64, uqa_core::Edge> = BTreeMap::new();
        for row in edge_rows {
            let properties: BTreeMap<String, uqa_core::Value> =
                serde_json::from_str(&row.properties_json)?;
            edge_by_id.insert(
                row.edge_id,
                uqa_core::Edge {
                    edge_id: row.edge_id,
                    source_id: row.source_id,
                    target_id: row.target_id,
                    label: row.label,
                    properties,
                },
            );
        }
        Ok((vertex_by_id, edge_by_id))
    }

    fn restore_graph_memberships(
        graphs: &mut BTreeMap<String, uqa_graph::MemoryGraphStore>,
        memberships: &[(String, u64, String)],
        vertex_by_id: &BTreeMap<u64, uqa_core::Vertex>,
        edge_by_id: &BTreeMap<u64, uqa_core::Edge>,
    ) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;

        // Validate every membership before mutating a graph, then
        // replay all vertex memberships before any edge membership. Catalog
        // ordering is not part of the persistence contract, and add_edge
        // correctly requires both endpoints to already belong to the target
        // graph.
        for (entity_type, entity_id, graph_name) in memberships {
            if !graphs.contains_key(graph_name) {
                return Err(StorageBackendError::Other(format!(
                    "graph membership references unregistered graph `{graph_name}`"
                )));
            }
            match entity_type.as_str() {
                "vertex" if vertex_by_id.contains_key(entity_id) => {}
                "vertex" => {
                    return Err(StorageBackendError::Other(format!(
                        "graph `{graph_name}` references missing vertex {entity_id}"
                    )));
                }
                "edge" if edge_by_id.contains_key(entity_id) => {}
                "edge" => {
                    return Err(StorageBackendError::Other(format!(
                        "graph `{graph_name}` references missing edge {entity_id}"
                    )));
                }
                other => {
                    return Err(StorageBackendError::Other(format!(
                        "graph `{graph_name}` has invalid membership type `{other}`"
                    )));
                }
            }
        }
        for (entity_type, entity_id, graph_name) in memberships {
            if entity_type != "vertex" {
                continue;
            }
            let store = graphs.get_mut(graph_name).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "graph membership references unregistered graph `{graph_name}`"
                ))
            })?;
            let vertex = vertex_by_id.get(entity_id).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "graph `{graph_name}` references missing vertex {entity_id}"
                ))
            })?;
            store
                .add_vertex(vertex.clone(), graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        for (entity_type, entity_id, graph_name) in memberships {
            if entity_type != "edge" {
                continue;
            }
            let store = graphs.get_mut(graph_name).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "graph membership references unregistered graph `{graph_name}`"
                ))
            })?;
            let edge = edge_by_id.get(entity_id).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "graph `{graph_name}` references missing edge {entity_id}"
                ))
            })?;
            store
                .add_edge(edge.clone(), graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        Ok(())
    }

    fn restore_graph_label_registries(
        graphs: &mut BTreeMap<String, uqa_graph::MemoryGraphStore>,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;

        // Restore the per-graph AGE label registries. The
        // persisted metadata is authoritative (it survives deletion of
        // every entity of a label); deriving label ids from existing
        // entity ids (`id >> 48`) self-heals missing metadata.
        for (graph_name, store) in graphs.iter_mut() {
            let vertices = store
                .vertex_ids_in_graph(graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            for edge in store
                .edges_in_graph(graph_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?
            {
                if !vertices.contains(&edge.source_id) || !vertices.contains(&edge.target_id) {
                    return Err(StorageBackendError::Other(format!(
                        "graph `{graph_name}` edge {} references missing endpoint {} -> {}",
                        edge.edge_id, edge.source_id, edge.target_id
                    )));
                }
            }
            let key = format!("{}{graph_name}", super::GRAPH_LABELS_METADATA_PREFIX);
            if let Some(json) = catalog.get_metadata(&key)? {
                if !json.is_empty() {
                    let registry = serde_json::from_str::<uqa_graph::GraphLabelRegistry>(&json)?;
                    store.import_label_registry(graph_name, &registry);
                }
            }
            store.rebuild_label_registry_from_ids(graph_name);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::Duration;

    use uqa_core::Value;

    use super::Engine;

    fn sqlite_data_version(engine: &Engine) -> u64 {
        engine
            .sqlite_session
            .as_ref()
            .expect("persistent test engine")
            .data_version()
            .expect("read SQLite data version")
            .expect("file-backed database has a data version")
    }

    #[test]
    fn contended_transaction_stack_does_not_hide_autocommit_data_generation() {
        let engine = Arc::new(Engine::new());
        let initial_epoch = engine
            .table_data_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let locked = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));

        let holder_engine = Arc::clone(&engine);
        let holder_locked = Arc::clone(&locked);
        let holder_release = Arc::clone(&release);
        let holder = std::thread::spawn(move || {
            let _guard = holder_engine.tx_stack.lock();
            holder_locked.wait();
            holder_release.wait();
        });
        locked.wait();

        let (done_tx, done_rx) = mpsc::channel();
        let notifier_engine = Arc::clone(&engine);
        let notifier = std::thread::spawn(move || {
            notifier_engine.note_table_data_changed();
            done_tx.send(()).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release.wait();
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        holder.join().unwrap();
        notifier.join().unwrap();

        assert_eq!(
            engine
                .table_data_epoch
                .load(std::sync::atomic::Ordering::Acquire),
            initial_epoch + 1
        );
        assert!(!engine
            .table_data_dirty
            .load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn pinned_and_rollback_reload_do_not_consume_late_legacy_sequences() {
        let directory = tempfile::tempdir().unwrap();
        let engine = Engine::open(&directory.path().join("late-legacy-sequence.db")).unwrap();
        let catalog = engine.catalog.as_ref().expect("persistent catalog");
        let legacy = r#"{"late":{"start":7,"increment":2,"current":5}}"#;
        catalog
            .set_metadata(crate::SEQUENCES_METADATA_KEY, legacy)
            .unwrap();
        let before = sqlite_data_version(&engine);

        engine.begin_implicit_statement_transaction(true).unwrap();
        assert_eq!(sqlite_data_version(&engine), before);
        engine.rollback().unwrap();

        assert_eq!(sqlite_data_version(&engine), before);
        assert_eq!(
            catalog
                .get_metadata(crate::SEQUENCES_METADATA_KEY)
                .unwrap()
                .as_deref(),
            Some(legacy)
        );
        assert!(catalog.load_sequence_rows().unwrap().is_empty());
    }

    #[test]
    fn pinned_reload_reports_a_missing_public_schema_without_repairing_it() {
        let directory = tempfile::tempdir().unwrap();
        let engine = Engine::open(&directory.path().join("missing-public.db")).unwrap();
        let catalog = engine.catalog.as_ref().expect("persistent catalog");
        catalog.drop_schema("public").unwrap();
        let before = sqlite_data_version(&engine);

        let error = engine
            .begin_implicit_statement_transaction(true)
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("missing required schema `public`"),
            "unexpected error: {error}"
        );
        assert_eq!(sqlite_data_version(&engine), before);
        assert!(!catalog
            .load_schemas()
            .unwrap()
            .iter()
            .any(|s| s == "public"));
    }

    #[test]
    fn legacy_fts_repair_is_one_time_and_reload_remains_read_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("one-time-fts-repair.db");
        {
            let engine = Engine::open(&path).unwrap();
            engine
                .sql(
                    "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT); \
                     INSERT INTO docs (id, body) VALUES (1, 'one-time repair'); \
                     CREATE INDEX docs_body_gin ON docs USING gin (body)",
                    &[],
                )
                .unwrap();
        }
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "DROP TABLE _postings; \
                 DROP TABLE _doc_lengths; \
                 DROP TABLE _field_stats;",
            )
            .unwrap();

        let engine = Engine::open(&path).unwrap();
        let hits = engine
            .sql("SELECT id FROM docs WHERE text_match(body, 'repair')", &[])
            .unwrap();
        assert_eq!(hits.rows[0].get("id"), Some(&Value::Int(1)));

        let after_initial_repair = sqlite_data_version(&engine);
        assert_eq!(
            engine
                .seen_sqlite_data_version
                .load(std::sync::atomic::Ordering::Acquire),
            after_initial_repair,
            "initial repair was committed after the monitor baseline"
        );

        engine.begin_implicit_statement_transaction(true).unwrap();
        engine.commit().unwrap();
        assert_eq!(
            sqlite_data_version(&engine),
            after_initial_repair,
            "pinned catalog reload repeated the FTS repair"
        );

        let external = rusqlite::Connection::open(&path).unwrap();
        external
            .execute(
                "INSERT OR REPLACE INTO _metadata (key, value) VALUES ('reload_probe', '1')",
                [],
            )
            .unwrap();
        let external_commit = sqlite_data_version(&engine);
        engine.synchronize_catalog_registries().unwrap();
        assert_eq!(
            sqlite_data_version(&engine),
            external_commit,
            "external-commit refresh repeated the FTS repair"
        );
    }
}
