//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, SQLError, SQLParam, SQLResult, Statement, UnifiedPlanExecutor};

pub(crate) fn estimate_engine_plan(
    engine: &Engine,
    plan: &uqa_planner::UnifiedPlan,
) -> Result<uqa_planner::plan_cost::PlanCost, SQLError> {
    uqa_planner::statement_planning::estimate_plan(engine.statement_statistics_context(), plan)
}

#[cfg(test)]
use super::{compile, Arc};

#[cfg(test)]
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

/// Analyze executable statements before optimizer evaluation can raise SQL errors.
pub(crate) fn plan_for_execution(
    engine: &Engine,
    plan: uqa_planner::UnifiedPlan,
    params: &[SQLParam],
) -> Result<uqa_planner::UnifiedPlan, SQLError> {
    uqa_planner::statement_planning::executable::plan_for_execution(
        &engine.statement_planning_context(),
        plan,
        params,
    )
}

pub(crate) fn optimize_engine_query(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
) -> Result<uqa_planner::QueryPlan, SQLError> {
    uqa_planner::statement_planning::executable::optimize_query(
        &engine.statement_planning_context(),
        query,
    )
}

pub(crate) fn optimize_engine_plan(
    engine: &Engine,
    plan: uqa_planner::UnifiedPlan,
) -> Result<uqa_planner::UnifiedPlan, SQLError> {
    uqa_planner::statement_planning::executable::optimize_plan(
        &engine.statement_planning_context(),
        plan,
    )
}

/// Lower and execute an already-compiled statement through the same unified
/// plan entry point used by [`Engine::sql`]. SQL/PLpgSQL routine bodies call
/// this instead of retaining a private AST dispatcher.
pub(crate) fn execute_compiled_statement(
    engine: &Engine,
    statement: Statement,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let plan = uqa_planner::UnifiedPlan::lower_with(statement, &|name: &str| {
        engine.has_registered_aggregate_function(name)
    });
    let plan = plan_for_execution(engine, plan, params)?;
    UnifiedPlanExecutor::new_nested(engine, params).execute(&plan)
}

pub(crate) fn execute_compiled_statement_with_privilege_subject(
    engine: &Engine,
    statement: Statement,
    params: &[SQLParam],
    privilege_subject: &str,
) -> Result<SQLResult, SQLError> {
    let mut plan = uqa_planner::UnifiedPlan::lower_with(statement, &|name: &str| {
        engine.has_registered_aggregate_function(name)
    });
    super::catalog_statement_routines::mark_catalog_statement_relations_bound(&mut plan)?;
    let plan = plan_for_execution(engine, plan, params)?;
    UnifiedPlanExecutor::new_nested(engine, params)
        .with_privilege_subject(privilege_subject)
        .execute(&plan)
}
