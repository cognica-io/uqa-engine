//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Child-query execution, output rewrites, and expression services for CTEs.

use crate::{
    query::{output::QueryOutput, runtime::QueryRuntimeView, scope::AfterEventFiring, CteScope},
    OwnedPhysicalRow,
};
use uqa_sql::{
    expr::EngineHook,
    plan::{CommandPlan, QueryPlan},
    routines::RoutineResolution,
    SQLError, SQLParam, SQLResult, ScalarExpr,
};

pub trait CteBodyExecutor<S: Clone>: Sync {
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
    ) -> Result<SQLResult, SQLError>;
    /// Fire the AFTER events the commands of a statement queued, in queue order, once the statement has finished.
    fn fire_after_events(&self, events: Vec<AfterEventFiring>) -> Result<(), SQLError>;
}

pub trait QueryOutputRewriter: Sync {
    /// Return a plan that applies the complete output predicate, or `None` when the caller must retain it.
    fn push_output_filter(
        &self,
        query: &QueryPlan,
        qualifier: &str,
        filter: &ScalarExpr,
        output_columns: Option<&[String]>,
    ) -> Result<Option<QueryPlan>, SQLError>;
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
