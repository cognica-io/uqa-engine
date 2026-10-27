//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared definition lifecycle and SQL versus host argument namespaces.

use super::UnifiedPlanExecutor;
use uqa_sql::{
    plan::{ExpressionPlan, UnifiedPlan},
    SQLError, SQLParam, SQLResult,
};

impl<S: Clone + Send + Sync + 'static> UnifiedPlanExecutor<'_, '_, S> {
    pub(super) fn execute_prepare(
        &self,
        name: &str,
        parameter_types: &[uqa_sql::ast::ColumnType],
        body: &UnifiedPlan,
    ) -> Result<SQLResult, SQLError> {
        if self.context.prepared.state.lookup_prepared(name).is_some() {
            return Err(uqa_sql::prepared::statement_error(
                "42P05",
                name,
                "already exists",
            ));
        }
        crate::statement::prepared::register_plan(
            &self.context.prepared.definitions,
            name.to_string(),
            body.clone(),
            parameter_types,
            self.source_sql.as_deref(),
        )?;
        Ok(SQLResult::empty())
    }

    pub(super) fn execute_prepared(
        &self,
        name: &str,
        params: &[ExpressionPlan],
    ) -> Result<SQLResult, SQLError> {
        let bound = crate::query::prepared::bind_execute_parameters(
            self.context.prepared.arguments,
            name,
            self.context.prepared.state.prepared_parameter_types(name),
            params,
        )?;
        self.execute_bound_prepared(name, &bound, true, None)
    }

    pub(crate) fn execute_host_prepared(&self, name: &str) -> Result<SQLResult, SQLError> {
        let bound = crate::query::prepared::bind_host_parameters(
            self.context.prepared.arguments,
            name,
            self.context.prepared.state.prepared_parameter_types(name),
            self.params,
        )?;
        self.execute_bound_prepared(
            name,
            &bound,
            self.nested_statement,
            self.source_sql.as_deref(),
        )
    }

    fn execute_bound_prepared(
        &self,
        name: &str,
        bound: &[SQLParam],
        nested: bool,
        source_sql: Option<&str>,
    ) -> Result<SQLResult, SQLError> {
        let plan = self
            .context
            .prepared
            .plans
            .plan_for_execution(name, bound)?
            .ok_or_else(|| uqa_sql::prepared::statement_error("26000", name, "does not exist"))?;
        UnifiedPlanExecutor::with_nested_statement(self.context.clone(), bound, nested)
            .with_optional_source_sql(source_sql)
            .execute(&plan)
    }

    pub(super) fn execute_deallocate(&self, name: Option<&str>) -> Result<SQLResult, SQLError> {
        if let Some(name) = name {
            if self.context.prepared.state.lookup_prepared(name).is_none() {
                return Err(uqa_sql::prepared::statement_error(
                    "26000",
                    name,
                    "does not exist",
                ));
            }
        }
        self.context.prepared.state.deallocate_prepared(name);
        Ok(SQLResult::empty())
    }
}
