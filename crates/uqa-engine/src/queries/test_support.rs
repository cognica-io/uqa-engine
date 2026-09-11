//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::Arc;
use uqa_sql::{compile, plan::QueryPlan, SQLError, Statement};

pub(super) fn compile_logical_plans(
    engine: &Engine,
    sql: &str,
) -> Result<Vec<uqa_planner::UnifiedPlan>, SQLError> {
    if let Some(cached) = engine.cached_sql_statement(sql) {
        return Ok(vec![cached.logical_plan.as_ref().clone()]);
    }
    let statements = compile(sql)?;
    let plans = statements
        .iter()
        .cloned()
        .map(|statement| lower_statement(engine, statement))
        .collect::<Vec<_>>();
    if plans.len() == 1 {
        engine.cache_sql_statement(
            sql.to_string(),
            Arc::new(statements[0].clone()),
            Arc::new(plans[0].clone()),
        );
    }
    Ok(plans)
}

pub(super) fn lower_statement(engine: &Engine, statement: Statement) -> uqa_planner::UnifiedPlan {
    uqa_planner::UnifiedPlan::lower_with(statement, &|name: &str| {
        engine.has_registered_aggregate_function(name)
    })
}

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
