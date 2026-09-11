//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose planner and SQL plan-effect analysis for the active session.

use crate::Engine;
use uqa_sql::{plan::QueryPlan, SQLError};

pub(super) fn query_may_mutate_engine(engine: &Engine, plan: &QueryPlan) -> Result<bool, SQLError> {
    uqa_sql::semantics::effects::query_may_mutate_engine(&engine.query_effect_context(), plan)
}

pub(super) fn query_requires_statement_transaction(
    engine: &Engine,
    plan: &QueryPlan,
) -> Result<bool, SQLError> {
    uqa_sql::semantics::effects::query_requires_statement_transaction(
        &engine.query_effect_context(),
        plan,
    )
}
