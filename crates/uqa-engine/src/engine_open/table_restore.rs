//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial table catalog migration and persistent table hydration.

use super::{
    Analyzer, Arc, AtomicBool, BTreeMap, CatalogFacade, Engine, FieldName,
    PersistentStorageBackend, RwLock, StorageBackendResult, TableSchema, TableState, VectorIndex,
};
use crate::{VectorIndexOpenMode, VectorIndexSpec};

impl Engine {
    pub(super) fn restore_from_catalog(
        &mut self,
        catalog: &dyn CatalogFacade,
        backend: &dyn PersistentStorageBackend,
    ) -> StorageBackendResult<()> {
        self.restore_schemas_from_catalog(catalog)?;
        let schemas = catalog.load_tables()?;
        for schema in schemas {
            let relation = schema.relation.clone();
            let table = Self::load_session_table(catalog, backend, schema)?;
            self.storage.tables.write().insert(relation, table);
        }
        self.restore_graphs_from_catalog(catalog)?;
        self.restore_engine_registries_from_catalog(catalog)?;
        Ok(())
    }

    /// Perform catalog mutations that are permitted only while opening a new
    /// engine session. Every later snapshot/reload path is deliberately
    /// load-only so a read transaction or rollback cannot commit a repair.
    pub(super) fn prepare_catalog_for_initial_restore(
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
        Self::migrate_constraint_names_from_metadata(catalog)?;
        Self::migrate_legacy_sequences_from_metadata(catalog)
    }

    fn migrate_constraint_names_from_metadata(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for mut schema in catalog.load_tables()? {
            let mut columns: Vec<uqa_sql::ast::ColumnDef> = if schema.columns_json.is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&schema.columns_json)?
            };
            let mut constraints: uqa_sql::ast::TableConstraintSet =
                if schema.constraints_json.is_empty() {
                    uqa_sql::ast::TableConstraintSet::default()
                } else {
                    serde_json::from_str(&schema.constraints_json)?
                };
            if crate::engine_table_storage::materialize_constraint_names(
                &schema.relation,
                &mut columns,
                &mut constraints,
            )? {
                schema.columns_json = serde_json::to_string(&columns)?;
                schema.constraints_json = serde_json::to_string(&constraints)?;
                catalog.save_table(&schema)?;
            }
        }
        Ok(())
    }

    pub(super) fn load_session_table(
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
                    VectorIndexSpec::BruteForce,
                    VectorIndexOpenMode::Restore,
                )?,
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
            hierarchy: RwLock::new(constraints.hierarchy),
            value_indexes: RwLock::new(BTreeMap::new()),
            doc_count_cache: std::sync::atomic::AtomicU64::new(0),
            doc_count_dirty: AtomicBool::new(true),
            persistence: constraints.persistence,
            on_commit: constraints.on_commit,
        }))
    }
}
