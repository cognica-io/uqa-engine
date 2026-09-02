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

fn metadata_foreign_keys(
    columns: &[uqa_sql::ast::ColumnDef],
    constraints: &uqa_sql::ast::TableConstraintSet,
) -> Vec<uqa_sql::ast::ForeignKey> {
    let mut foreign_keys = constraints.foreign_keys.clone();
    for column in columns {
        let Some(reference) = &column.references else {
            continue;
        };
        foreign_keys.push(uqa_sql::ast::ForeignKey {
            name: reference.name.clone(),
            object_id: reference.object_id,
            local_columns: vec![column.name.clone()],
            ref_table: reference.table.clone(),
            ref_columns: reference.column.clone().into_iter().collect(),
            on_update: reference.on_update,
            on_delete: reference.on_delete,
            on_delete_set_columns: Vec::new(),
            match_type: reference.match_type,
            enforced: reference.enforced,
            validated: reference.validated,
            deferrable: reference.deferrable,
            initially_deferred: reference.initially_deferred,
            period: reference.period,
        });
    }
    foreign_keys
}

struct ConstraintMetadataMigration {
    schema: TableSchema,
    columns: Vec<uqa_sql::ast::ColumnDef>,
    constraints: uqa_sql::ast::TableConstraintSet,
    changed: bool,
}

fn load_constraint_metadata_migrations(
    catalog: &dyn CatalogFacade,
) -> StorageBackendResult<Vec<ConstraintMetadataMigration>> {
    let mut migrations = Vec::new();
    for schema in catalog.load_tables()? {
        let mut columns = if schema.columns_json.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&schema.columns_json)?
        };
        let mut constraints = if schema.constraints_json.is_empty() {
            uqa_sql::ast::TableConstraintSet::default()
        } else {
            serde_json::from_str(&schema.constraints_json)?
        };
        let dispatches_changed =
            crate::engine_table_storage::upgrade_legacy_schema_function_dispatches(
                &mut columns,
                &mut constraints,
            );
        let metadata_changed = crate::engine_table_storage::materialize_constraint_metadata(
            &schema.relation,
            &mut columns,
            &mut constraints,
        )?;
        migrations.push(ConstraintMetadataMigration {
            schema,
            columns,
            constraints,
            changed: dispatches_changed || metadata_changed,
        });
    }
    Ok(migrations)
}

fn inherited_parent_object_id(
    parent_foreign_keys: &BTreeMap<String, Vec<uqa_sql::ast::ForeignKey>>,
    parents: &[String],
    inherited: &uqa_sql::ast::ForeignKey,
) -> Option<[u8; 16]> {
    parents.iter().find_map(|parent| {
        parent_foreign_keys.get(parent).and_then(|foreign_keys| {
            foreign_keys
                .iter()
                .find(|foreign_key| {
                    crate::engine_table_storage::foreign_keys_match_without_object_id(
                        foreign_key,
                        inherited,
                    )
                })
                .and_then(|foreign_key| foreign_key.object_id)
        })
    })
}

fn apply_inherited_object_id(
    migration: &mut ConstraintMetadataMigration,
    inherited_index: usize,
    object_id: [u8; 16],
) -> bool {
    let inherited = migration
        .constraints
        .hierarchy
        .partition_inherited_foreign_keys[inherited_index]
        .clone();
    let mut changed = false;
    if inherited.object_id != Some(object_id) {
        migration
            .constraints
            .hierarchy
            .partition_inherited_foreign_keys[inherited_index]
            .object_id = Some(object_id);
        changed = true;
    }
    if let Some(foreign_key) = migration
        .constraints
        .foreign_keys
        .iter_mut()
        .find(|foreign_key| {
            crate::engine_table_storage::foreign_keys_match_without_object_id(
                foreign_key,
                &inherited,
            )
        })
    {
        if foreign_key.object_id != Some(object_id) {
            foreign_key.object_id = Some(object_id);
            changed = true;
        }
    }
    migration.changed |= changed;
    changed
}

fn synchronize_inherited_constraint_object_ids(migrations: &mut [ConstraintMetadataMigration]) {
    for _ in 0..migrations.len() {
        let parent_foreign_keys = migrations
            .iter()
            .map(|migration| {
                (
                    migration.schema.relation.qualified_name(),
                    metadata_foreign_keys(&migration.columns, &migration.constraints),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut pass_changed = false;
        for migration in &mut *migrations {
            let parents = migration.constraints.hierarchy.parents.clone();
            let inherited_count = migration
                .constraints
                .hierarchy
                .partition_inherited_foreign_keys
                .len();
            for inherited_index in 0..inherited_count {
                let inherited = &migration
                    .constraints
                    .hierarchy
                    .partition_inherited_foreign_keys[inherited_index];
                let Some(object_id) =
                    inherited_parent_object_id(&parent_foreign_keys, &parents, inherited)
                else {
                    continue;
                };
                pass_changed |= apply_inherited_object_id(migration, inherited_index, object_id);
            }
        }
        if !pass_changed {
            break;
        }
    }
}

fn save_constraint_metadata_migrations(
    catalog: &dyn CatalogFacade,
    migrations: Vec<ConstraintMetadataMigration>,
) -> StorageBackendResult<()> {
    for mut migration in migrations {
        if !migration.changed {
            continue;
        }
        migration.schema.columns_json = serde_json::to_string(&migration.columns)?;
        migration.schema.constraints_json = serde_json::to_string(&migration.constraints)?;
        catalog.save_table(&migration.schema)?;
    }
    Ok(())
}

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
        let schemas = catalog.load_schema_rows()?;
        for schema in &schemas {
            Self::validate_schema_name(&schema.name)?;
        }
        if !schemas.iter().any(|schema| schema.name == "public") {
            catalog.save_schema_row(&uqa_storage::SchemaRow::legacy("public"))?;
        }
        Self::migrate_table_identities(catalog)?;
        Self::repair_dangling_hierarchy_parents(catalog)?;
        Self::migrate_constraint_names_from_metadata(catalog)?;
        Self::migrate_legacy_sequences_from_metadata(catalog)?;
        Self::migrate_sequence_identities(catalog)?;
        Self::migrate_implicit_sequence_owners(catalog)
    }

    fn migrate_table_identities(catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
        for mut schema in catalog.load_tables()? {
            let mut changed = false;
            if schema.object_id == [0; 16] {
                schema.object_id = crate::new_table_object_id()?;
                changed = true;
            }
            if schema.storage_generation == [0; 16] {
                schema.storage_generation = crate::new_table_storage_generation()?;
                changed = true;
            }
            if !changed {
                continue;
            }
            catalog.save_table(&schema)?;
        }
        Ok(())
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
            if parents == hierarchy.parents {
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
        let mut migrations = load_constraint_metadata_migrations(catalog)?;
        synchronize_inherited_constraint_object_ids(&mut migrations);
        save_constraint_metadata_migrations(catalog, migrations)
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
            lifecycle_id: std::sync::atomic::AtomicU64::new(crate::next_table_lifecycle_id()),
            object_id: schema.object_id,
            role_owner: RwLock::new(schema.role_owner),
            storage_generation: RwLock::new(schema.storage_generation),
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
