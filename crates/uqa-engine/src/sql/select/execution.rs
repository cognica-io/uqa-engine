//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind the session's statement generation for execution-owned query processing.
use super::{
    CteScope, Engine, QueryOutput, QueryOutputMode, QueryPlan, SQLError, SQLParam, SQLResult,
};
pub(in crate::sql) fn execute_query_plan_with_ctes(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    uqa_execution::query::statement::execute_query_plan_with_ctes(
        &engine.statement_execution_context(),
        plan,
        params,
        ctes,
    )
}
pub(in crate::sql) fn execute_query_plan_output(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    uqa_execution::query::statement::execute_query_plan_output(
        &engine.statement_execution_context(),
        plan,
        params,
        ctes,
        output_mode,
    )
}
