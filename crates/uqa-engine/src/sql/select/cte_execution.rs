//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind child CTE execution to the active engine statement.
use super::{
    execute_lateral_subquery_output, execute_query_plan_output, push_output_filter_into_query_plan,
    CtePlan, CteScope, Engine, QueryOutput, QueryOutputMode, QueryPlan, SQLError, SQLParam,
    SQLResult, ScalarExpr,
};
use crate::session::StatementReadSnapshot;
use uqa_execution::query::cte::context::{
    CteBodyExecutor, CteExecutionContext, QueryOutputRewriter,
};
pub(in crate::sql) use uqa_planner::explain::*;

impl Engine {
    pub(crate) fn cte_execution_context(&self) -> CteExecutionContext<'_, StatementReadSnapshot> {
        CteExecutionContext {
            queries: self,
            rewrites: self,
            routines: self,
            functions: self,
            runtime: self.query_runtime_view(),
        }
    }
}
impl CteBodyExecutor<StatementReadSnapshot> for Engine {
    fn execute_query(
        &self,
        query: &QueryPlan,
        params: &[SQLParam],
        ctes: &mut CteScope,
    ) -> Result<QueryOutput, SQLError> {
        execute_query_plan_output(self, query, params, ctes, QueryOutputMode::SharedSpill)
    }
    fn execute_lateral_query(
        &self,
        query: &QueryPlan,
        outer: &uqa_execution::OwnedPhysicalRow,
        params: &[SQLParam],
        ctes: &CteScope,
    ) -> Result<QueryOutput, SQLError> {
        execute_lateral_subquery_output(self, query, outer, params, ctes)
    }
    fn execute_command(
        &self,
        command: &uqa_planner::CommandPlan,
        params: &[SQLParam],
        ctes: &CteScope,
    ) -> Result<SQLResult, SQLError> {
        crate::sql::dml::execute_cte_command(self, command, params, ctes)
    }
}
impl QueryOutputRewriter for Engine {
    fn push_output_filter(
        &self,
        query: &QueryPlan,
        qualifier: &str,
        filter: &ScalarExpr,
        columns: Option<&[String]>,
    ) -> Result<Option<QueryPlan>, SQLError> {
        push_output_filter_into_query_plan(self, query, qualifier, filter, columns)
    }
}
pub(in crate::sql) fn materialize_plan_ctes(
    engine: &Engine,
    plans: &[CtePlan],
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<(), SQLError> {
    uqa_execution::query::cte::materialize_plan_ctes(
        engine.cte_execution_context(),
        plans,
        params,
        ctes,
    )
}
