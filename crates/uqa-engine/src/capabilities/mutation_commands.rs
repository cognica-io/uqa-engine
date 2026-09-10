//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind transaction command state and storage point mutations for execution.
use crate::{session::StatementReadSnapshot, Engine};
use std::collections::BTreeMap;
use uqa_core::{DocId, Value};
use uqa_execution::mutation::{
    command_scope::MutationCommandState,
    point_update::{context::PointMutationStorage, RowUpdateVectors},
};
use uqa_sql::SQLError;
impl MutationCommandState for Engine {
    fn prepare_writer(&self) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer().map(|_| ())
    }
    fn begin_overlay(&self) {
        self.mutation_coordinator().begin_command_mutation_overlay();
    }
    fn end_overlay(&self) {
        self.mutation_coordinator().end_command_mutation_overlay();
    }
}
impl PointMutationStorage for Engine {
    fn find_doc_id_by_field(
        &self,
        table: &str,
        field: &str,
        value: &Value,
    ) -> Result<Option<DocId>, SQLError> {
        Engine::find_doc_id_by_field(self, table, field, value)
    }
    fn patch_document_fields_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &RowUpdateVectors,
    ) -> Result<bool, SQLError> {
        Engine::patch_document_fields_with_vector_values(self, table, doc_id, updates, vectors)
    }
}

impl uqa_sql::semantics::mutation_qualifiers::MutationTargetColumns for Engine {
    fn try_query_table_columns(&self, table: &str) -> Result<Vec<String>, String> {
        Engine::try_query_table_columns(self, table).map_err(|error| error.to_string())
    }
}
impl Engine {
    pub(crate) fn mutation_execution_context(
        &self,
    ) -> uqa_execution::mutation::statement::MutationExecutionContext<'_, StatementReadSnapshot>
    {
        uqa_execution::mutation::statement::MutationExecutionContext {
            preparation: self.mutation_preparation_context(),
            identities: self.insert_identity_context(),
            rules: self.view_rule_execution_context(),
            publication: self.mutation_publication_context(),
            state: self,
            targets: self,
            scopes: self,
            points: self,
            privileges: self,
        }
    }
}

impl uqa_sql::semantics::mutation_privileges::MutationPrivilegeCatalog for Engine {
    fn bound_table_column_names(&self, table: &str) -> Result<Vec<String>, SQLError> {
        Engine::bound_table_column_names(self, table)
    }
    fn ensure_table_privilege_for(
        &self,
        table: &str,
        subject: &str,
        privilege: uqa_sql::catalog::security::table::TableAclPrivilege,
    ) -> Result<(), SQLError> {
        Engine::ensure_table_privilege_for(self, table, subject, privilege)
    }
    fn ensure_column_privilege_for(
        &self,
        table: &str,
        column: &str,
        subject: &str,
        privilege: uqa_sql::catalog::security::table::TableAclPrivilege,
    ) -> Result<(), SQLError> {
        Engine::ensure_column_privilege_for(self, table, column, subject, privilege)
    }
    fn ensure_any_column_privilege_for(
        &self,
        table: &str,
        subject: &str,
        privilege: uqa_sql::catalog::security::table::TableAclPrivilege,
    ) -> Result<(), SQLError> {
        Engine::ensure_any_column_privilege_for(self, table, subject, privilege)
    }
}
