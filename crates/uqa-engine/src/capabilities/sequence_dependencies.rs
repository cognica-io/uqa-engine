//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lend real table/column and sequence identity guards to native dependency consumers.
use crate::{Engine, TableState};
use std::sync::Arc;
use uqa_execution::{
    catalog::foreign::reads::ForeignTablesRead,
    schema::sequences::dependency_lifecycle::{
        SequenceChecksRead, SequenceColumnsRead, SequenceDependencyCatalog,
        SequenceDependencyContext, SequenceTableMetadata,
    },
};
use uqa_sql::schema::sequences::{
    dependencies::analysis::{SequenceExpressionCatalog, SequenceExpressionObjectIdsRead},
    dependents::SequenceSchemaDependent,
};
use uqa_storage::{SequenceOwner, StorageBackendResult};
impl SequenceTableMetadata for TableState {
    fn object_id(&self) -> [u8; 16] {
        self.object_id()
    }
    fn columns(&self) -> SequenceColumnsRead<'_> {
        Box::new(self.columns.read())
    }
    fn table_checks(&self) -> SequenceChecksRead<'_> {
        Box::new(self.table_checks.read())
    }
}
impl SequenceDependencyCatalog for Engine {
    fn refresh_tables(&self) -> StorageBackendResult<()> {
        self.synchronize_table_catalog()
    }
    fn refresh_catalog(&self) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()
    }
    fn table_entries(&self) -> Vec<(String, Arc<dyn SequenceTableMetadata>)> {
        Engine::table_entries(self)
            .into_iter()
            .map(|(name, state)| (name, state as Arc<dyn SequenceTableMetadata>))
            .collect()
    }
    fn resolve_table_name(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.try_resolve_table_name(name)
    }
    fn table(&self, name: &str) -> StorageBackendResult<Option<Arc<dyn SequenceTableMetadata>>> {
        self.try_table(name)
            .map(|state| state.map(|state| state as Arc<dyn SequenceTableMetadata>))
    }
    fn foreign_tables(&self) -> ForeignTablesRead<'_> {
        Box::new(self.durable.foreign_tables.read())
    }
}
impl SequenceExpressionCatalog for Engine {
    fn object_ids(&self) -> SequenceExpressionObjectIdsRead<'_> {
        Box::new(self.durable.sequence_object_ids.read())
    }
}
impl Engine {
    pub(crate) fn sequence_dependency_context(&self) -> SequenceDependencyContext<'_> {
        SequenceDependencyContext {
            catalog: self,
            sequences: self,
            expressions: self,
            views: self.view_dependency_context(),
            events: self.event_lookup_context(),
            foreign: self.foreign_definition_context(),
            tables: self,
            changes: self,
            constraints: self.constraint_alter_context(),
            schema: self.schema_publication_context(),
            columns: self.column_drop_publication_context(),
        }
    }
    pub(crate) fn sequence_names_owned_by_tables(
        &self,
        table_object_ids: &std::collections::BTreeSet<[u8; 16]>,
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        uqa_execution::catalog::sequence_introspection::ownership::sequence_names_owned_by_tables(
            self,
            table_object_ids,
        )
    }
    pub(crate) fn sequence_names_owned_by_column(
        &self,
        table_object_id: [u8; 16],
        column_object_id: [u8; 16],
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        uqa_execution::catalog::sequence_introspection::ownership::sequence_names_owned_by_column(
            self,
            table_object_id,
            column_object_id,
        )
    }
    pub(crate) fn sequence_schema_expression_dependents(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<Vec<SequenceSchemaDependent>> {
        self.sequence_dependency_context()
            .sequence_schema_expression_dependents(sequence)
    }
    pub(crate) fn resolve_stored_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        self.sequence_dependency_context()
            .resolve_stored_sequence_references_in_expr(expression)
    }
    pub(crate) fn sequence_external_dependents_for_owner_drop(
        &self,
        sequence: &str,
        owner_drop_targets: &std::collections::BTreeSet<String>,
    ) -> StorageBackendResult<Vec<String>> {
        self.sequence_dependency_context()
            .sequence_external_dependents_for_owner_drop(sequence, owner_drop_targets)
    }
    pub(crate) fn owned_sequence_dependents_for_column(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Vec<String>> {
        self.sequence_dependency_context()
            .owned_sequence_dependents_for_column(table_name, column_name)
    }
    pub(crate) fn sequence_owner_target(
        &self,
        owner: SequenceOwner,
    ) -> Option<(String, String, bool)> {
        self.sequence_dependency_context()
            .sequence_owner_target(owner)
    }
}
