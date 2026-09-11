//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind schema publication to one retained table generation and its catalog backend.
use crate::{Engine, TableState};
use std::sync::Arc;
use uqa_execution::schema::publication::{
    SchemaPublicationContext, TableSchemaCatalog, TableSchemaState,
};
use uqa_sql::ast::{ColumnDef, TableConstraintSet};
use uqa_storage::StorageBackendResult;

pub(crate) use uqa_execution::catalog::identity::allocate_catalog_object_id;
impl Engine {
    pub(crate) fn schema_publication_context(&self) -> SchemaPublicationContext<'_> {
        SchemaPublicationContext {
            catalog: self,
            types: self,
            bindings: self.schema_dependency_binding_context(),
            allocate_identity: allocate_catalog_object_id,
        }
    }
}
struct SchemaTableBinding<'a> {
    engine: &'a Engine,
    name: String,
    state: Arc<TableState>,
}
impl TableSchemaCatalog for Engine {
    fn resolve_table_name(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.try_resolve_table_name(name)
    }
    fn table_state(
        &self,
        canonical: &str,
    ) -> StorageBackendResult<Option<Box<dyn TableSchemaState + '_>>> {
        self.try_table(canonical).map(|state| {
            state.map(|state| {
                Box::new(SchemaTableBinding {
                    engine: self,
                    name: canonical.to_string(),
                    state,
                }) as Box<dyn TableSchemaState>
            })
        })
    }
}
impl TableSchemaState for SchemaTableBinding<'_> {
    fn dependency_constraints(&self) -> TableConstraintSet {
        TableConstraintSet {
            checks: self.state.table_checks.read().clone(),
            foreign_keys: self.state.foreign_keys.read().clone(),
            key_constraints: self.state.key_constraints.read().clone(),
            hierarchy: self.state.hierarchy.read().clone(),
            columns_declared: None,
            persistence: self.state.persistence,
            on_commit: self.state.on_commit,
        }
    }
    fn publish_expressions(&self, columns: &[ColumnDef], checks: &[uqa_sql::ast::TableCheck]) {
        columns.clone_into(&mut *self.state.columns.write());
        checks.clone_into(&mut *self.state.table_checks.write());
    }

    fn write_columns(
        &self,
    ) -> Box<dyn uqa_execution::schema::publication::columns::ColumnSchemaWrite + '_> {
        Box::new(SchemaColumnWrite {
            columns: self.state.columns.write(),
        })
    }
    fn persist_columns(&self, columns: &[ColumnDef]) -> StorageBackendResult<()> {
        if self.engine.is_persistent() {
            self.engine
                .try_save_table_schema_with_columns(&self.name, &self.state, columns)?;
        }
        Ok(())
    }
    fn constraint_header(&self) -> TableConstraintSet {
        TableConstraintSet {
            columns_declared: Some(*self.state.columns_declared.read()),
            persistence: self.state.persistence,
            on_commit: self.state.on_commit,
            hierarchy: self.state.hierarchy.read().clone(),
            ..TableConstraintSet::default()
        }
    }
    fn publish_constraints(&self, columns: Vec<ColumnDef>, constraints: TableConstraintSet) {
        *self.state.columns.write() = columns;
        *self.state.table_checks.write() = constraints.checks;
        *self.state.foreign_keys.write() = constraints.foreign_keys;
        *self.state.key_constraints.write() = constraints.key_constraints;
    }

    fn key_constraints(&self) -> Vec<uqa_sql::ast::TableKeyConstraint> {
        self.state.key_constraints.read().clone()
    }
    fn hierarchy(&self) -> uqa_sql::ast::TableHierarchy {
        self.state.hierarchy.read().clone()
    }
    fn publish_hierarchy(&self, hierarchy: uqa_sql::ast::TableHierarchy) {
        *self.state.hierarchy.write() = hierarchy;
    }
    fn columns(&self) -> Vec<ColumnDef> {
        self.state.columns.read().clone()
    }
    fn constraints(&self) -> TableConstraintSet {
        TableConstraintSet {
            columns_declared: Some(*self.state.columns_declared.read()),
            persistence: self.state.persistence,
            on_commit: self.state.on_commit,
            checks: self.state.table_checks.read().clone(),
            foreign_keys: self.state.foreign_keys.read().clone(),
            key_constraints: self.state.key_constraints.read().clone(),
            hierarchy: self.state.hierarchy.read().clone(),
        }
    }
    fn columns_declared(&self) -> bool {
        *self.state.columns_declared.read()
    }
    fn mark_statistics_dirty(&self) -> StorageBackendResult<()> {
        self.engine.mark_column_stats_dirty(&self.name, &self.state)
    }
    fn persist_candidate(
        &self,
        columns: &[ColumnDef],
        constraints: &TableConstraintSet,
    ) -> StorageBackendResult<()> {
        if self.engine.is_persistent() {
            self.engine.try_save_table_schema_with_components(
                &self.name,
                &self.state,
                columns,
                constraints,
            )?;
        }
        Ok(())
    }
    fn publish_columns(
        &self,
        columns_declared: bool,
        columns: Vec<ColumnDef>,
        constraints: TableConstraintSet,
    ) {
        *self.state.columns_declared.write() = columns_declared;
        *self.state.columns.write() = columns;
        *self.state.table_checks.write() = constraints.checks;
        *self.state.foreign_keys.write() = constraints.foreign_keys;
        *self.state.key_constraints.write() = constraints.key_constraints;
    }
    fn persist_next_id(&self) -> StorageBackendResult<()> {
        self.engine.persist_next_id(&self.name)
    }
    fn refresh_value_indexes(&self) -> StorageBackendResult<()> {
        self.engine.refresh_value_indexes_for_table(&self.name)
    }
}

impl uqa_execution::schema::publication::SchemaWriteTransaction for Engine {
    fn with_schema_write(
        &self,
        write: uqa_execution::schema::publication::SchemaWrite<'_>,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| write(&engine.schema_publication_context()))
    }
}

struct SchemaColumnWrite<'a> {
    columns: parking_lot::MappedRwLockWriteGuard<'a, Vec<ColumnDef>>,
}
impl uqa_execution::schema::publication::columns::ColumnSchemaWrite for SchemaColumnWrite<'_> {
    fn columns(&self) -> &[ColumnDef] {
        &self.columns
    }
    fn publish(&mut self, columns: Vec<ColumnDef>) {
        *self.columns = columns;
    }
}

impl Engine {
    pub(crate) fn schema_dependency_publication_context(
        &self,
    ) -> uqa_execution::schema::publication::dependencies::SchemaDependencyPublicationContext<'_>
    {
        uqa_execution::schema::publication::dependencies::SchemaDependencyPublicationContext {
            tables: self,
            foreign: self,
            changes: self,
        }
    }
}
impl uqa_execution::schema::publication::dependencies::LoadedTableSchemas for Engine {
    fn table_schemas(
        &self,
    ) -> Vec<uqa_execution::schema::publication::dependencies::LoadedTableSchema<'_>> {
        self.table_entries()
            .into_iter()
            .map(|(name, state)| {
                (
                    name.clone(),
                    Box::new(SchemaTableBinding {
                        engine: self,
                        name,
                        state,
                    }) as Box<dyn TableSchemaState>,
                )
            })
            .collect()
    }
}
impl uqa_execution::schema::publication::dependencies::ForeignSchemaPublication for Engine {
    fn foreign_tables(
        &self,
    ) -> std::collections::BTreeMap<
        uqa_core::RelationIdentity,
        uqa_execution::catalog::foreign::StoredForeignTable,
    > {
        self.durable.foreign_tables.read().clone()
    }
    fn persist_foreign_table(
        &self,
        relation: &uqa_core::RelationIdentity,
        table: &uqa_execution::catalog::foreign::StoredForeignTable,
    ) -> StorageBackendResult<()> {
        self.foreign_definition_context()
            .persist_foreign_table_definition(relation, table)
    }
    fn publish_foreign_tables(
        &self,
        updates: Vec<(
            uqa_core::RelationIdentity,
            uqa_execution::catalog::foreign::StoredForeignTable,
        )>,
    ) {
        let mut tables = self.durable.foreign_tables.write();
        for (relation, table) in updates {
            tables.insert(relation, table);
        }
    }
}
impl uqa_execution::schema::publication::dependencies::CatalogPublicationChanges for Engine {
    fn table_catalog_changed(&self) {
        self.note_table_catalog_changed();
    }
    fn catalog_registry_changed(&self) {
        self.note_catalog_registry_changed();
    }
}

impl Engine {
    pub(crate) fn view_sequence_rewrite_context(
        &self,
    ) -> uqa_execution::schema::sequences::dependencies::ViewSequenceRewriteContext<'_> {
        uqa_execution::schema::sequences::dependencies::ViewSequenceRewriteContext {
            views: self,
            changes: self,
        }
    }
}
impl uqa_execution::schema::sequences::dependencies::ViewCatalogPublication for Engine {
    fn synchronize_catalog(&self) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()
    }
    fn view_definitions(
        &self,
    ) -> uqa_execution::schema::sequences::dependencies::ViewDefinitionsRead<'_> {
        Box::new(self.durable.views.read())
    }
    fn has_catalog(&self) -> bool {
        self.storage.catalog.is_some()
    }
    fn save_view_row(&self, row: &uqa_storage::ViewRow) -> StorageBackendResult<()> {
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.save_view(row)?;
        }
        Ok(())
    }
    fn publish_views(
        &self,
        updates: Vec<(
            uqa_core::RelationIdentity,
            uqa_sql::catalog::stored_view::StoredView,
        )>,
    ) {
        self.durable.views.write().extend(updates);
    }
}
