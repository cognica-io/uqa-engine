//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture metadata and statement scopes for RETURNING analysis and row projection.
use crate::{session::StatementReadSnapshot, Engine};
use uqa_execution::mutation::returning::ReturningExecutionContext;
use uqa_sql::{
    ast::ColumnDef,
    binding::snapshot::BindingSnapshot,
    semantics::returning::{ReturningAnalysisContext, ReturningCatalog, ReturningScope},
    RowSchema, SQLError,
};
impl ReturningCatalog for Engine {
    fn try_describe_table_row_type(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Engine::try_describe_table_row_type(self, table).map_err(|error| error.to_string())
    }
    fn try_table_columns(&self, table: &str) -> Result<Vec<String>, String> {
        Engine::try_table_columns(self, table).map_err(|error| error.to_string())
    }
    fn view_schema(&self, table: &str) -> Result<Option<RowSchema>, SQLError> {
        Engine::view_schema(self, table)
    }
}
impl ReturningScope for Engine {
    fn binding_snapshot(&self) -> Result<BindingSnapshot, SQLError> {
        let scope = super::query_scope::new_for_current_routine(self);
        uqa_execution::query::binding::binding_context(&scope).map(BindingSnapshot::from)
    }
}
impl Engine {
    pub(crate) fn returning_analysis_context(&self) -> ReturningAnalysisContext<'_> {
        ReturningAnalysisContext {
            catalog: self,
            routines: self,
            aggregates: self,
            scope: self,
        }
    }
    pub(crate) fn returning_execution_context(
        &self,
    ) -> ReturningExecutionContext<'_, StatementReadSnapshot> {
        ReturningExecutionContext {
            rows: self.mutation_row_context(),
            catalog: self,
            routines: self,
            relational: self.relational_context(),
        }
    }
}

impl Engine {
    pub(crate) fn cursor_command_returning_schema(
        &self,
        command: &uqa_sql::plan::CommandPlan,
        params: &[uqa_sql::SQLParam],
    ) -> Result<Option<RowSchema>, SQLError> {
        uqa_execution::mutation::entry::cursor_command_returning_schema(
            &self.returning_execution_context(),
            self.returning_analysis_context(),
            self,
            command,
            params,
        )
    }
}
