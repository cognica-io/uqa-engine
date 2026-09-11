//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL SELECT, set-operation, `CtePlan`, ordering, and projection execution.

use uqa_execution::ScalarExpr;
use uqa_planner::{QueryBlockPlan, QueryPlan};

use super::from_rows::execute_lateral_subquery_output;
use super::{Engine, SQLError, SQLParam, SQLResult};

mod cte_execution;
mod filter_pushdown;
mod physical_plan;
mod row_lock_retry_cache;
mod row_locking;
mod schema_binding;
mod set_projection;

pub(crate) use crate::capabilities::query_scope::CteScope;
pub(in crate::sql) use filter_pushdown::*;
pub(in crate::sql) use physical_plan::*;
pub(crate) use row_lock_retry_cache::RowLockRetryCache;
pub(in crate::sql) use schema_binding::*;

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

pub(super) type QueryOutputMode<'consumer> =
    uqa_execution::query::statement::consumer::QueryOutputMode<
        'consumer,
        crate::session::StatementReadSnapshot,
    >;
pub(in crate::sql) use uqa_execution::query::output::QueryOutput;

/// Execute a physical query plan while preserving the caller's CTE scope.
mod execution;
pub(super) use execution::execute_query_plan_output;

pub(crate) use crate::capabilities::ScopedEngineHook;
pub(crate) use row_locking::attach_lock_rows;
