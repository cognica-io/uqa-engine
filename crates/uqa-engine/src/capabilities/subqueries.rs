//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lend statement query contexts and scoped callbacks to subquery execution.

use crate::{
    capabilities::{query_scope::CteScope, ScopedEngineHook},
    session::StatementReadSnapshot,
    Engine,
};
use uqa_core::Value;
use uqa_execution::{
    query::{
        sources::SourceContext,
        statement::context::QueryContext,
        subqueries::{
            context::{ScopedSubqueryHooks, SubqueryProbe, SubqueryQueryContexts},
            SubqueryServices,
        },
    },
    scalar::plan::{PhysicalOuterRow, PhysicalSubqueryRunner},
    SubqueryResult,
};
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam};

impl Engine {
    pub(crate) fn subquery_services(&self) -> SubqueryServices<'_, StatementReadSnapshot> {
        SubqueryServices {
            catalog: self,
            session: self,
            volatility: self,
            queries: self,
            hooks: self,
        }
    }
}

impl SubqueryQueryContexts<StatementReadSnapshot> for Engine {
    fn query_context(&self) -> QueryContext<'_, StatementReadSnapshot> {
        self.query_execution_context()
    }
    fn source_context(&self) -> SourceContext<'_, StatementReadSnapshot> {
        self.source_execution_context()
    }
}

impl ScopedSubqueryHooks<StatementReadSnapshot> for Engine {
    fn with_hooks(&self, scope: &CteScope, probe: SubqueryProbe<'_>) -> Result<bool, SQLError> {
        let hook = ScopedEngineHook::new(self, scope);
        probe(&hook, &hook)
    }
}

impl PhysicalSubqueryRunner for ScopedEngineHook<'_> {
    fn execute_subquery(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError> {
        self.subquery_context()
            .execute_subquery(subquery, plan, outer_row, params)
    }
    fn scalar_subquery_value(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        self.subquery_context()
            .scalar_subquery_value(subquery, plan, outer_row, params)
    }
    fn subquery_exists(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        self.subquery_context()
            .subquery_exists(subquery, plan, outer_row, params)
    }
    fn subquery_contains(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        needle: &Value,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        self.subquery_context()
            .subquery_contains(subquery, plan, needle, outer_row, params)
    }
}
