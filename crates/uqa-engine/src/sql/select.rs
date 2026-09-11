//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL SELECT, set-operation, `CtePlan`, ordering, and projection execution.

use uqa_execution::ScalarExpr;
use uqa_planner::{QueryBlockPlan, QueryPlan};

use super::from_rows::execute_lateral_subquery_output;
use super::scalar::{
    eval_physical_scalar, PhysicalEvalContext, PhysicalOuterRow, PhysicalSubqueryRunner,
};
use super::volatility::query_contains_volatile_function;
use super::{Engine, SQLError, SQLParam, SQLResult, Value};

mod cte_execution;
mod evaluation;
mod filter_pushdown;
mod physical_plan;
mod row_lock_retry_cache;
mod row_locking;
mod schema_binding;
mod set_projection;

pub(in crate::sql) use cte_execution::*;
pub(crate) use evaluation::CteScope;
pub(in crate::sql) use filter_pushdown::*;
pub(in crate::sql) use physical_plan::*;
pub(crate) use row_lock_retry_cache::RowLockRetryCache;
pub(in crate::sql) use row_locking::*;
pub(in crate::sql) use schema_binding::*;

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

/// Execute the physical relational plan directly. CTEs, set-operation branches, values, and query blocks recurse through plan children; query blocks select physical access and row operators without reconstructing a parser statement.
pub(crate) fn execute_query_plan(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut ctes = crate::capabilities::query_scope::new_for_current_routine(engine);
    execute_query_plan_with_ctes(engine, plan, params, &mut ctes)
}

pub(super) type QueryOutputMode<'consumer> =
    uqa_execution::query::statement::consumer::QueryOutputMode<
        'consumer,
        crate::session::StatementReadSnapshot,
    >;
pub(in crate::sql) use uqa_execution::query::consumer::QueryConsumerControl;
pub(in crate::sql) use uqa_execution::query::output::{QueryOutput, QueryRows};

/// Execute a physical query plan while preserving the caller's CTE scope.
mod execution;
pub(super) use execution::{execute_query_plan_output, execute_query_plan_with_ctes};

#[cfg(test)]
mod physical_failure_tests;

pub(crate) use evaluation::{prepare_correlated_exists_predicate, ScopedEngineHook};
pub(crate) use row_locking::attach_lock_rows;
