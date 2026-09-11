//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply active session capabilities to physical tuple locking.

use super::{CteScope, Engine, QueryBlockPlan, SQLError, SQLParam};
use uqa_execution::PhysicalOperator;
pub(in crate::sql) type LockRowsRecheckSource =
    uqa_execution::query::recheck_source::LockRowsRecheckSource<
        crate::session::StatementReadSnapshot,
    >;

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
