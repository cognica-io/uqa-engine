//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind declared columns, routine scopes and identity state for mutation execution.
use crate::Engine;
use uqa_execution::mutation::{
    assignment::MutationAssignmentContext,
    identity::{IdentitySequences, InsertIdentityCatalog, InsertIdentityContext},
};
use uqa_sql::{
    assignment::columns::AssignmentColumnCatalog,
    ast::{AutoIncrement, ColumnDef, Expr},
    SQLError,
};
impl AssignmentColumnCatalog for Engine {
    fn try_describe_table(
        &self,
        table: &str,
    ) -> Result<Option<Vec<ColumnDef>>, uqa_sql::assignment::columns::ColumnCatalogError> {
        Engine::try_describe_table(self, table)
            .map_err(|error| Box::new(error) as uqa_sql::assignment::columns::ColumnCatalogError)
    }
    fn columns_declared(
        &self,
        table: &str,
    ) -> Result<bool, uqa_sql::assignment::columns::ColumnCatalogError> {
        self.try_table(table)
            .map(|table| table.is_some_and(|table| *table.columns_declared.read()))
            .map_err(|error| Box::new(error) as uqa_sql::assignment::columns::ColumnCatalogError)
    }
    fn try_column_insert_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Option<Expr>, uqa_sql::assignment::columns::ColumnCatalogError> {
        Engine::try_column_insert_default_expr(self, table, column)
            .map_err(|error| Box::new(error) as uqa_sql::assignment::columns::ColumnCatalogError)
    }
}
impl InsertIdentityCatalog for Engine {
    fn auto_increment_column(&self, table: &str) -> Result<Option<String>, String> {
        Engine::auto_increment_column(self, table).map_err(|error| error.to_string())
    }
    fn auto_increment_columns(&self, table: &str) -> Result<Vec<(String, AutoIncrement)>, String> {
        Engine::auto_increment_columns(self, table).map_err(|error| error.to_string())
    }
}
impl IdentitySequences for Engine {
    fn nextval_sql(&self, sequence: &str) -> Result<i64, SQLError> {
        Engine::nextval_sql(self, sequence)
    }
}
impl Engine {
    pub(crate) fn mutation_assignment_context(
        &self,
    ) -> MutationAssignmentContext<'_, crate::session::StatementReadSnapshot> {
        MutationAssignmentContext {
            columns: self,
            assignment: self,
            rows: self.mutation_row_context(),
            expressions: self.mutation_expression_context(),
            scopes: self,
        }
    }
    pub(crate) fn insert_identity_context(&self) -> InsertIdentityContext<'_> {
        InsertIdentityContext {
            catalog: self,
            columns: self,
            assignment: self,
            identifiers: self,
            sequences: self,
            locks: self,
            partitions: self,
        }
    }
}

impl Engine {
    pub(crate) fn mutation_preparation_context(
        &self,
    ) -> uqa_execution::mutation::preparation::MutationPreparationContext<
        '_,
        crate::session::StatementReadSnapshot,
    > {
        uqa_execution::mutation::preparation::MutationPreparationContext {
            referential: self.referential_execution_context(),
            staging: self.mutation_staging_context(),
            returning: self.returning_execution_context(),
        }
    }
}
