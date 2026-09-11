//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply native schema contexts at each statement and transaction boundary.

use crate::{session::StatementReadSnapshot, Engine};
use uqa_execution::statement::context::schemas::{
    SchemaOwnerTransactions, SchemaOwnerWrite, SchemaStatementInputs,
};
use uqa_sql::{SQLError, SQLResult};
impl SchemaStatementInputs<StatementReadSnapshot> for Engine {
    fn index_creation_context(
        &self,
    ) -> uqa_execution::schema::indexes::creation::IndexCreationContext<'_> {
        Engine::index_creation_context(self)
    }
    fn table_alter_entry_context(
        &self,
    ) -> uqa_execution::schema::table_alteration::entry::TableAlterEntryContext<
        '_,
        StatementReadSnapshot,
    > {
        Engine::table_alter_entry_context(self)
    }
    fn domain_creation_context(&self) -> uqa_execution::schema::domains::DomainCreationContext<'_> {
        Engine::domain_creation_context(self)
    }
    fn schema_creation_context(
        &self,
    ) -> uqa_execution::schema::namespaces::SchemaCreationContext<'_> {
        Engine::schema_creation_context(self)
    }
    fn schema_privilege_context(
        &self,
    ) -> uqa_execution::schema::namespaces::privileges::SchemaPrivilegeContext<'_> {
        Engine::schema_privilege_context(self)
    }
    fn database_privilege_context(
        &self,
    ) -> uqa_execution::catalog::security::database_lifecycle::DatabasePrivilegeContext<'_> {
        Engine::database_privilege_context(self)
    }
    fn sequence_privilege_context(
        &self,
    ) -> uqa_execution::catalog::security::sequence_lifecycle::SequencePrivilegeContext<'_> {
        Engine::sequence_privilege_context(self)
    }
    fn vacuum_execution_context(&self) -> uqa_execution::maintenance::VacuumContext<'_> {
        Engine::vacuum_execution_context(self)
    }
    fn truncate_context(&self) -> uqa_execution::schema::truncate::TruncateContext<'_> {
        Engine::truncate_context(self)
    }
}
impl SchemaOwnerTransactions for Engine {
    fn with_owner_write(&self, operation: SchemaOwnerWrite<'_>) -> Result<SQLResult, SQLError> {
        self.with_implicit_transaction(|engine| operation(&engine.schema_owner_context()))
    }
}
