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
    fn with_table_row_type_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(&[ColumnDef]) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        self.synchronize_table_catalog()
            .and_then(|()| self.synchronize_table_data())
            .map_err(|error| {
                match uqa_execution::storage_errors::storage_error("read table row type", &error) {
                    SQLError::Internal(_) => match control.format(format_args!("{error}")) {
                        Ok(message) => SQLError::Internal(message.into_parts().0),
                        Err(error) => error.into(),
                    },
                    diagnostic => diagnostic,
                }
            })?;
        let candidates =
            uqa_sql::catalog::resolution::candidates::relation_lookup_candidates_with_control(
                self, name, control,
            )?;
        let table = {
            let tables = self.storage.tables.read();
            candidates
                .iter()
                .find_map(|relation| tables.get(relation).cloned())
        };
        let Some(table) = table else {
            return Ok(false);
        };
        control.check()?;
        read(&table.columns.read())?;
        Ok(true)
    }
    fn try_table_columns(&self, table: &str) -> Result<Vec<String>, String> {
        Engine::table_columns_in_execution(self, table).map_err(|error| error.to_string())
    }
    fn visit_column_names_with_control(
        &self,
        table: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(&str) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        self.visit_raw_table_column_names_with_control(table, control, read)
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
            destination: None,
            rows: self.mutation_row_context(),
            catalog: self,
            routines: self,
            relational: self.relational_context(),
        }
    }
}

impl uqa_execution::mutation::returning::ReturningInputs<StatementReadSnapshot> for Engine {
    fn returning_execution_context(
        &self,
    ) -> uqa_execution::mutation::returning::ReturningExecutionContext<'_, StatementReadSnapshot>
    {
        Engine::returning_execution_context(self)
    }
    fn returning_analysis_context(
        &self,
    ) -> uqa_sql::semantics::returning::ReturningAnalysisContext<'_> {
        Engine::returning_analysis_context(self)
    }
}
