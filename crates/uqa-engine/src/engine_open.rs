//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    normalize_analyzer_phase, Analyzer, Arc, AtomicBool, BTreeMap, Catalog, CatalogFacade,
    ColumnStatsRow, DeepModel, Engine, FieldName, IVFIndexParams, ManagedConnection, Path,
    PersistentStorageBackend, RwLock, SQLiteCompressionOptions, SQLiteError, SQLiteStorageBackend,
    StorageBackendError, StorageBackendResult, TableState, Value, VectorIndex,
};

impl Engine {
    pub fn open(path: &Path) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open(path)?;
        Self::open_with_connection(&conn)
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
        Self::from_persistent_backends(catalog, backend).map_err(Self::sqlite_open_error)
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
        let mut engine = Self {
            tables: RwLock::new(BTreeMap::new()),
            catalog: Some(catalog),
            backend: Some(backend),
            graphs: RwLock::new(BTreeMap::new()),
            models: RwLock::new(BTreeMap::new()),
            scoring_params: RwLock::new(BTreeMap::new()),
            views: RwLock::new(BTreeMap::new()),
            catalog_indexes: RwLock::new(BTreeMap::new()),
            schemas: RwLock::new(std::collections::BTreeSet::new()),
            search_path: RwLock::new(vec!["public".to_string()]),
            session_vars: RwLock::new(BTreeMap::new()),
            path_indexes: RwLock::new(BTreeMap::new()),
            tx_stack: parking_lot::Mutex::new(Vec::new()),
            cancel: uqa_core::CancellationToken::new(),
            sequences: RwLock::new(BTreeMap::new()),
            prepared: RwLock::new(BTreeMap::new()),
            sql_statement_cache: RwLock::new(super::SQLStatementCache::default()),
            named_analyzers: RwLock::new(BTreeMap::new()),
            table_field_analyzers: RwLock::new(BTreeMap::new()),
            foreign_servers: RwLock::new(BTreeMap::new()),
            foreign_tables: RwLock::new(BTreeMap::new()),
            foreign_memory_tables: RwLock::new(BTreeMap::new()),
            sql_scalar_functions: RwLock::new(BTreeMap::new()),
            sql_table_functions: RwLock::new(BTreeMap::new()),
            sql_aggregate_functions: RwLock::new(BTreeMap::new()),
        };
        let catalog = engine.catalog.as_ref().expect("persistent catalog").clone();
        let backend = engine.backend.as_ref().expect("persistent backend").clone();
        engine.restore_from_catalog(catalog.as_ref(), backend.as_ref())?;
        // Eagerly populate the model cache from the catalog so
        // `load_model` is one read deep.
        if let Ok(rows) = catalog.load_models() {
            for (name, json) in rows {
                if let Ok(model) = serde_json::from_str::<DeepModel>(&json) {
                    engine.models.write().insert(name, model);
                }
            }
        }
        Ok(engine)
    }

    fn sqlite_open_error(err: StorageBackendError) -> SQLiteError {
        match err {
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
        let schemas = catalog.load_tables()?;
        for schema in schemas {
            let analyzer: Analyzer = serde_json::from_str(&schema.analyzer_json)?;
            let docs = backend.document_store(&schema.name);
            let inv = backend.inverted_index(&schema.name, analyzer.clone());
            let mut vectors: BTreeMap<FieldName, Box<dyn VectorIndex>> = BTreeMap::new();
            for vf in &schema.vector_fields {
                vectors.insert(
                    vf.field.clone(),
                    backend.vector_index(&schema.name, &vf.field, vf.dimensions, None),
                );
            }
            let columns: Vec<uqa_sql::ast::ColumnDef> = if schema.columns_json.is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&schema.columns_json).unwrap_or_default()
            };
            // Restore the per-table id watermark to one past the largest
            // existing doc id so reopened catalogs do not collide on
            // SERIAL/BIGSERIAL columns.
            let max_id = { docs.max_doc_id() };
            let table = TableState {
                document_store: RwLock::new(docs),
                inverted_index: RwLock::new(inv),
                vector_indexes: RwLock::new(vectors),
                fts_fields: RwLock::new(schema.fts_fields.clone()),
                columns: RwLock::new(columns),
                next_id: parking_lot::Mutex::new(max_id + 1),
                analyzer: RwLock::new(analyzer),
                column_stats: RwLock::new(BTreeMap::new()),
                column_stats_loaded: AtomicBool::new(false),
                column_stats_dirty: AtomicBool::new(false),
                table_checks: RwLock::new(Vec::new()),
                foreign_keys: RwLock::new(Vec::new()),
            };
            self.tables.write().insert(schema.name, Arc::new(table));
        }
        self.restore_graphs_from_catalog(catalog)?;
        self.restore_engine_registries_from_catalog(catalog)?;
        Ok(())
    }

    pub(crate) fn load_column_stats_from_catalog(
        catalog: &dyn CatalogFacade,
        table_name: &str,
    ) -> StorageBackendResult<BTreeMap<String, uqa_planner::ColumnStats>> {
        let mut out = BTreeMap::new();
        for row in catalog.load_column_stats(table_name)? {
            out.insert(row.column_name.clone(), Self::column_stats_from_row(row));
        }
        Ok(out)
    }

    fn column_stats_from_row(row: ColumnStatsRow) -> uqa_planner::ColumnStats {
        uqa_planner::ColumnStats {
            distinct_count: row.distinct_count.try_into().unwrap_or(0),
            null_count: row.null_count.try_into().unwrap_or(0),
            min_value: Self::decode_column_stat_value(row.min_value),
            max_value: Self::decode_column_stat_value(row.max_value),
            row_count: row.row_count.try_into().unwrap_or(0),
            histogram: serde_json::from_str(&row.histogram_json).unwrap_or_default(),
            mcv_values: serde_json::from_str(&row.mcv_values_json).unwrap_or_default(),
            mcv_frequencies: serde_json::from_str(&row.mcv_frequencies_json).unwrap_or_default(),
        }
    }

    fn decode_column_stat_value(raw: Option<String>) -> Option<Value> {
        let raw = raw?;
        match serde_json::from_str::<Value>(&raw) {
            Ok(Value::Null) => None,
            Ok(v) => Some(v),
            Err(_) => Some(Value::Str(raw)),
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
        self.restore_sequences_from_metadata(catalog)?;
        self.restore_views_from_metadata(catalog)?;
        self.restore_analyzers_from_catalog(catalog)?;
        self.restore_foreign_registries_from_catalog(catalog)?;
        self.restore_catalog_indexes_from_catalog(catalog)?;
        self.restore_path_indexes_from_catalog(catalog)?;
        Ok(())
    }

    fn restore_analyzers_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (name, config_json) in catalog.load_analyzers()? {
            self.named_analyzers.write().insert(name, config_json);
        }
        for (table, field, phase, analyzer_name) in catalog.load_table_field_analyzers()? {
            if let (Some(t), Ok(analyzer), Ok((phase_name, phase))) = (
                self.table(&table),
                self.resolve_analyzer(&analyzer_name),
                normalize_analyzer_phase(&phase),
            ) {
                let _ = t
                    .inverted_index
                    .write()
                    .set_field_analyzer(&field, analyzer, phase);
                self.table_field_analyzers
                    .write()
                    .insert((table, field), (analyzer_name, phase_name));
                continue;
            }
            self.table_field_analyzers
                .write()
                .insert((table, field), (analyzer_name, phase));
        }
        Ok(())
    }

    fn restore_foreign_registries_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (name, fdw_type, options_json) in catalog.load_foreign_servers()? {
            let options: BTreeMap<String, String> =
                serde_json::from_str(&options_json).unwrap_or_default();
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
            let columns: Vec<uqa_sql::ast::ColumnDef> =
                serde_json::from_str(&row.columns_json).unwrap_or_default();
            let options: BTreeMap<String, String> =
                serde_json::from_str(&row.options_json).unwrap_or_default();
            let fdw_columns: Vec<uqa_fdw::ColumnDef> = columns
                .iter()
                .map(|c| uqa_fdw::ColumnDef {
                    name: c.name.clone(),
                    ty: match &c.ty {
                        uqa_sql::ast::ColumnType::Integer => uqa_fdw::ColumnType::Integer,
                        uqa_sql::ast::ColumnType::Real
                        | uqa_sql::ast::ColumnType::Numeric { .. } => uqa_fdw::ColumnType::Real,
                        uqa_sql::ast::ColumnType::Text
                        | uqa_sql::ast::ColumnType::Json
                        | uqa_sql::ast::ColumnType::Date
                        | uqa_sql::ast::ColumnType::Time
                        | uqa_sql::ast::ColumnType::TimeTz
                        | uqa_sql::ast::ColumnType::Timestamp
                        | uqa_sql::ast::ColumnType::TimestampTz => uqa_fdw::ColumnType::Text,
                        uqa_sql::ast::ColumnType::Bytea
                        | uqa_sql::ast::ColumnType::Vector(_)
                        | uqa_sql::ast::ColumnType::Tensor(_) => uqa_fdw::ColumnType::Bytes,
                    },
                })
                .collect();
            self.foreign_tables.write().insert(
                row.name.clone(),
                uqa_fdw::ForeignTable {
                    name: row.name,
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
            if !self.has_table(&row.table_name) {
                continue;
            }
            self.catalog_indexes
                .write()
                .insert(row.name.clone(), row.clone());
            let columns: Vec<String> = serde_json::from_str(&row.columns_json).unwrap_or_default();
            let parameters: BTreeMap<String, String> =
                serde_json::from_str(&row.parameters_json).unwrap_or_default();
            if row.index_type.eq_ignore_ascii_case("gin") {
                let analyzer = parameters
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("analyzer"))
                    .map(|(_, v)| v.as_str());
                for col in &columns {
                    let _ = self.restore_fts_field_from_catalog(&row.table_name, col, analyzer);
                }
                if catalog.fts_storage_was_reset() {
                    if let Some(table) = self.table(&row.table_name) {
                        Self::rebuild_fts_index(&table).map_err(StorageBackendError::Other)?;
                    }
                }
            } else if row.index_type.eq_ignore_ascii_case("ivf")
                || row.index_type.eq_ignore_ascii_case("hnsw")
            {
                let params = IVFIndexParams::from_map_lossy(&parameters);
                for col in &columns {
                    if let Some(
                        uqa_sql::ast::ColumnType::Vector(dim)
                        | uqa_sql::ast::ColumnType::Tensor(dim),
                    ) = self.column_type(&row.table_name, col)
                    {
                        let _ = self.restore_ivf_vector_field(&row.table_name, col, dim, params);
                    }
                }
            }
        }
        Ok(())
    }

    fn restore_path_indexes_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (key, seq_json) in catalog.load_path_indexes()? {
            let label_sequences: Vec<Vec<String>> =
                serde_json::from_str(&seq_json).unwrap_or_default();
            if let Some((graph, _name)) = key.split_once("::") {
                let graphs = self.graphs.read();
                let Some(store) = graphs.get(graph) else {
                    continue;
                };
                let idx = uqa_graph::PathIndex::build(store, graph, &label_sequences);
                drop(graphs);
                self.path_indexes.write().insert(key.clone(), idx);
            }
        }
        Ok(())
    }

    fn restore_graphs_from_catalog(&self, catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
        use std::collections::BTreeMap;
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
        // Step 2: load every vertex / edge into a side-table keyed
        // by global id. Memberships drive which graphs each entity
        // ends up attached to.
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
        // Step 3: replay each membership row through the per-graph
        // store. add_vertex / add_edge populate the partition's
        // adjacency indexes for free.
        for (entity_type, entity_id, graph_name) in catalog.load_graph_memberships()? {
            let store = graphs.entry(graph_name.clone()).or_default();
            if !store.has_graph(&graph_name) {
                store.create_graph(&graph_name);
            }
            match entity_type.as_str() {
                "vertex" => {
                    if let Some(v) = vertex_by_id.get(&entity_id) {
                        store.add_vertex(v.clone(), &graph_name);
                    }
                }
                "edge" => {
                    if let Some(e) = edge_by_id.get(&entity_id) {
                        store.add_edge(e.clone(), &graph_name);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
