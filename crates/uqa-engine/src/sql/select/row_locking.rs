//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply active session capabilities to physical tuple locking.

use super::{CteScope, Engine, QueryBlockPlan, QueryPlan, SQLError, SQLParam};
pub(in crate::sql) use uqa_execution::query::locking::query_has_row_locks;
use uqa_execution::PhysicalOperator;
pub(in crate::sql) type LockRowsRecheckSource =
    uqa_execution::query::recheck_source::LockRowsRecheckSource<
        crate::session::StatementReadSnapshot,
    >;

pub(in crate::sql) fn lock_query_relations(
    engine: &Engine,
    query: &QueryPlan,
) -> Result<(), SQLError> {
    uqa_execution::query::locking::lock_query_relations(engine.row_lock_context(), query)
}

pub(in crate::sql) fn validate_query_row_locks(
    engine: &Engine,
    query: &QueryPlan,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    uqa_execution::query::locking::validate_query_row_locks(
        engine.row_lock_context(),
        query,
        params,
    )
}

pub(crate) fn attach_lock_rows<'a>(
    engine: &'a Engine,
    operator: Box<dyn PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &CteScope,
    max_rows: Option<u64>,
    recheck_source: Option<LockRowsRecheckSource>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    uqa_execution::query::locking::attach_lock_rows(
        engine.row_lock_context(),
        operator,
        statement,
        params,
        ctes,
        max_rows,
        recheck_source,
    )
}
