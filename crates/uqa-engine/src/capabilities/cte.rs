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
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam, ScalarExpr};

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
    fn read_control(&self) -> Result<uqa_storage::read_control::StorageReadControl, SQLError> {
        self.query_retention_control()
    }
    fn retained_query(
        &self,
        query: &QueryPlan,
        params: &[SQLParam],
        ctes: &CteScope,
    ) -> Result<Option<Box<dyn uqa_execution::PhysicalOperator>>, SQLError> {
        self.retained_query_operator(query.clone(), params.to_vec(), ctes.clone())
    }
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
    ) -> Result<uqa_execution::mutation::output::MutationOutput, SQLError> {
        uqa_execution::mutation::entry::execute_cte_command(
            &self.mutation_entry_context(),
            command,
            params,
            ctes,
        )
    }
    fn fire_after_triggers(
        &self,
        queue: &uqa_execution::mutation::triggers::queue::AfterTriggerQueue,
    ) -> Result<(), SQLError> {
        queue.fire(&self.trigger_execution_context())
    }
}
impl QueryOutputRewriter for Engine {
    fn prune_query_outputs(
        &self,
        query: &mut QueryPlan,
        aliases: &[String],
        projection: &uqa_sql::plan::source_projection::SourceProjection,
        schema: &uqa_sql::RowSchema,
        visible_ctes: &dyn Fn(&str) -> bool,
    ) -> Result<(), SQLError> {
        uqa_planner::optimizer::prune_query_outputs(
            query,
            aliases,
            &|name| projection.contains(name),
            Some(schema),
            uqa_planner::optimizer::OutputPruningContext {
                aggregates: self,
                routines: Some(self),
                volatility: Some(self),
                visible_ctes,
            },
        )
    }

    fn optimize_retained_query(
        &self,
        query: QueryPlan,
    ) -> Result<uqa_sql::plan::OwnedQueryPlan, SQLError> {
        super::statement_planning::optimize_engine_query(self, std::borrow::Cow::Owned(query))
    }

    fn push_output_filter(
        &self,
        query: &QueryPlan,
        qualifier: &str,
        filter: &ScalarExpr,
        columns: Option<&[String]>,
    ) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError> {
        super::query_planning::push_output_filter_into_query_plan(
            self, query, qualifier, filter, columns,
        )
    }

    fn push_derived_output_filter(
        &self,
        query: &QueryPlan,
        qualifier: &str,
        filter: &ScalarExpr,
        columns: Option<&[String]>,
    ) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError> {
        super::query_planning::push_derived_output_filter(self, query, qualifier, filter, columns)
    }
}
