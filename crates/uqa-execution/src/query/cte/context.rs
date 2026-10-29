//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Child-query execution, output rewrites, and expression services for CTEs.

use crate::{
    query::{output::QueryOutput, runtime::QueryRuntimeView, CteScope},
    OwnedPhysicalRow,
};
use uqa_sql::{
    expr::EngineHook,
    plan::{CommandPlan, QueryPlan},
    routines::RoutineResolution,
    SQLError, SQLParam, ScalarExpr,
};

pub trait CteBodyExecutor<S: Clone>: Sync {
    /// Retain the active query allowance and its original cancellation owner for CTE analysis and scheduling.
    fn read_control(&self) -> Result<uqa_storage::read_control::StorageReadControl, SQLError>;
    /// Retain one read-only physical producer and its selected session inputs. A semantic materialization that cannot expose a pull operator returns None.
    fn retained_query(
        &self,
        query: &QueryPlan,
        params: &[SQLParam],
        ctes: &CteScope<S>,
    ) -> Result<Option<Box<dyn crate::PhysicalOperator>>, SQLError>;
    fn execute_query(
        &self,
        query: &QueryPlan,
        params: &[SQLParam],
        ctes: &mut CteScope<S>,
    ) -> Result<QueryOutput, SQLError>;
    fn execute_lateral_query(
        &self,
        query: &QueryPlan,
        outer: &OwnedPhysicalRow,
        params: &[SQLParam],
        ctes: &CteScope<S>,
    ) -> Result<QueryOutput, SQLError>;
    fn execute_command(
        &self,
        command: &CommandPlan,
        params: &[SQLParam],
        ctes: &CteScope<S>,
    ) -> Result<crate::mutation::output::MutationOutput, SQLError>;
    /// Fire the AFTER events that the commands of a statement queued, once the statement has finished.
    fn fire_after_triggers(
        &self,
        queue: &crate::mutation::triggers::queue::AfterTriggerQueue,
    ) -> Result<(), SQLError>;
}

pub trait QueryOutputRewriter: Sync {
    /// Remove scalar output expressions the containing query does not need, preserving the stored row descriptor and side effects.
    fn prune_query_outputs(
        &self,
        query: &mut QueryPlan,
        aliases: &[String],
        projection: &uqa_sql::plan::source_projection::SourceProjection,
        schema: &crate::RowSchema,
        visible_ctes: &dyn Fn(&str) -> bool,
    ) -> Result<(), SQLError>;

    /// Consume the private catalog-query destination before assembling its streaming or materialized execution path. The caller has already restored layouts and applied its output demand.
    fn optimize_retained_query(
        &self,
        query: uqa_sql::plan::MutableQueryPlan,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::plan::OwnedQueryPlan, SQLError>;

    fn prune_owned_query_outputs(
        &self,
        query: uqa_sql::plan::MutableQueryPlan,
        aliases: &[String],
        projection: &uqa_sql::plan::source_projection::SourceProjection,
        schema: &crate::RowSchema,
        visible_ctes: &dyn Fn(&str) -> bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::plan::MutableQueryPlan, SQLError>;

    /// Return a plan that applies the complete output predicate, or `None` when the caller must retain it.
    fn push_output_filter(
        &self,
        query: &QueryPlan,
        qualifier: &str,
        filter: &ScalarExpr,
        output_columns: Option<&[String]>,
    ) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError>;

    /// Place a predicate inside an ordinary derived source. Recursive specialization keeps its separate effect boundary through `push_output_filter`.
    fn push_derived_output_filter(
        &self,
        query: &QueryPlan,
        qualifier: &str,
        filter: &ScalarExpr,
        output_columns: Option<&[String]>,
    ) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError> {
        self.push_output_filter(query, qualifier, filter, output_columns)
    }
}

#[derive(Clone)]
pub struct CteExecutionContext<'a, S: Clone> {
    pub queries: &'a dyn CteBodyExecutor<S>,
    pub rewrites: &'a dyn QueryOutputRewriter,
    pub routines: &'a dyn RoutineResolution,
    pub functions: &'a (dyn EngineHook + Sync),
    pub runtime: QueryRuntimeView<'a>,
}
impl<S: Clone> Copy for CteExecutionContext<'_, S> {}
