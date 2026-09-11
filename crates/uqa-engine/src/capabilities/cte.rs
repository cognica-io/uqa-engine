//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind child CTE execution to the active session and query scopes.

use crate::{capabilities::query_scope::CteScope, session::StatementReadSnapshot, Engine};
use uqa_execution::query::{
    cte::context::{CteBodyExecutor, CteExecutionContext, QueryOutputRewriter},
    output::QueryOutput,
    statement::consumer::QueryOutputMode,
};
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam, SQLResult, ScalarExpr};

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
        uqa_execution::query::statement::execute_query_plan_output(
            &self.query_execution_context(),
            query,
            params,
            ctes,
            QueryOutputMode::SharedSpill,
        )
    }
    fn execute_lateral_query(
        &self,
        query: &QueryPlan,
        outer: &uqa_execution::OwnedPhysicalRow,
        params: &[SQLParam],
        ctes: &CteScope,
    ) -> Result<QueryOutput, SQLError> {
        uqa_execution::query::sources::lateral_query::execute_lateral_subquery_output(
            &self.source_execution_context(),
            query,
            outer,
            params,
            ctes,
        )
    }
    fn execute_command(
        &self,
        command: &uqa_planner::CommandPlan,
        params: &[SQLParam],
        ctes: &CteScope,
    ) -> Result<SQLResult, SQLError> {
        uqa_execution::mutation::entry::execute_cte_command(
            &self.mutation_entry_context(),
            command,
            params,
            ctes,
        )
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
        super::query_planning::push_output_filter_into_query_plan(
            self, query, qualifier, filter, columns,
        )
    }
}
