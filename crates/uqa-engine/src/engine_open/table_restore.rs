//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial table catalog migration and persistent table hydration.

use std::collections::BTreeSet;

use super::{
    Analyzer, Arc, AtomicBool, BTreeMap, CatalogFacade, Engine, FieldName,
    PersistentStorageBackend, RwLock, StorageBackendError, StorageBackendResult, TableSchema,
    TableState, VectorIndex,
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
        self.synchronize_partition_identity_watermarks()?;
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
        Self::repair_dangling_hierarchy_parents(catalog)?;
        Self::migrate_legacy_sequences_from_metadata(catalog)
    }

    fn repair_dangling_hierarchy_parents(catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
        let schemas = catalog.load_tables()?;
        let existing = schemas
            .iter()
            .map(|schema| schema.relation.qualified_name())
            .collect::<BTreeSet<_>>();
        for mut schema in schemas {
            if schema.constraints_json.is_empty() {
                continue;
            }
            let mut constraints: uqa_sql::ast::TableConstraintSet =
                serde_json::from_str(&schema.constraints_json)?;
            let hierarchy = &mut constraints.hierarchy;
            let mut parents = Vec::with_capacity(hierarchy.parents.len());
            let mut sequence_numbers = Vec::with_capacity(hierarchy.parents.len());
            for (index, parent) in hierarchy.parents.iter().enumerate() {
                let canonical_parent = crate::RelationIdentity::from_legacy_name(parent)
                    .map_err(StorageBackendError::Other)?
                    .qualified_name();
                if existing.contains(&canonical_parent) {
                    parents.push(canonical_parent);
                    sequence_numbers.push(hierarchy.parent_sequence_number(index));
                }
            }
            if parents.len() == hierarchy.parents.len() {
                continue;
            }
            hierarchy.parents = parents;
            hierarchy.parent_sequence_numbers = sequence_numbers;
            if hierarchy.parents.is_empty() {
                let mut columns: Vec<uqa_sql::ast::ColumnDef> = if schema.columns_json.is_empty() {
                    Vec::new()
                } else {
                    serde_json::from_str(&schema.columns_json)?
                };
                if hierarchy.partition_bound.take().is_some() {
                    for identity_override in &hierarchy.partition_identity_overrides {
                        if let Some(column) = columns
                            .iter_mut()
                            .find(|column| column.name == identity_override.column)
                        {
                            column
                                .auto_increment
                                .clone_from(&identity_override.original);
                        }
                    }
                    for inherited in &hierarchy.partition_inherited_key_constraints {
                        if let Some(index) = constraints
                            .key_constraints
                            .iter()
                            .position(|constraint| constraint == inherited)
                        {
                            constraints.key_constraints.remove(index);
                        }
                    }
                    for inherited in &hierarchy.partition_inherited_foreign_keys {
                        if let Some(index) = constraints
                            .foreign_keys
                            .iter()
                            .position(|constraint| constraint == inherited)
                        {
                            constraints.foreign_keys.remove(index);
                        }
                    }
                    hierarchy.partition_identity_overrides.clear();
                    hierarchy.partition_inherited_key_constraints.clear();
                    hierarchy.partition_inherited_foreign_keys.clear();
                }
                hierarchy.local_columns =
                    columns.iter().map(|column| column.name.clone()).collect();
                schema.columns_json = serde_json::to_string(&columns)?;
            }
            schema.constraints_json = serde_json::to_string(&constraints)?;
            catalog.save_table(&schema)?;
        }
        Ok(())
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
        let persisted_next_id = if columns.iter().any(|column| {
            column
                .auto_increment
                .as_ref()
                .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy)
        }) {
            Self::load_persisted_next_id(catalog, &table_name)?
        } else {
            None
        };
        let next_id = persisted_next_id.unwrap_or(1).max(u128::from(max_id) + 1);
        Ok(Arc::new(TableState {
            document_store: RwLock::new(docs),
            inverted_index: RwLock::new(inv),
            vector_indexes: RwLock::new(vectors),
            fts_fields: RwLock::new(schema.fts_fields),
            columns: RwLock::new(columns),
            next_id: parking_lot::Mutex::new(next_id),
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
