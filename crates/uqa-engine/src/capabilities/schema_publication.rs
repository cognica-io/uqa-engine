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
use uqa_sql::schema::constraint_metadata::{ConstraintMetadataError, ConstraintMetadataResult};
use uqa_storage::StorageBackendResult;

pub(crate) fn allocate_catalog_object_id(kind: &str) -> ConstraintMetadataResult<[u8; 16]> {
    let mut object_id = [0_u8; 16];
    getrandom::fill(&mut object_id).map_err(|error| {
        ConstraintMetadataError(format!("allocate {kind} object identity: {error}"))
    })?;
    Ok(object_id)
}
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
