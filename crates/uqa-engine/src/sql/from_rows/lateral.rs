//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind correlated query invocation to the current statement's services.

use super::{CteScope, Engine, QueryOutput, QueryPlan, SQLError, SQLParam};

pub(in crate::sql) fn execute_lateral_subquery_output(
    engine: &Engine,
    plan: &QueryPlan,
    outer: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<QueryOutput, SQLError> {
    uqa_execution::query::sources::lateral_query::execute_lateral_subquery_output(
        &engine.source_execution_context(),
        plan,
        outer,
        params,
        ctes,
    )
}
