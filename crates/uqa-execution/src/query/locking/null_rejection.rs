//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply immutable statement binding inputs for SQL outer-join null rejection.

use super::RowLockContext;
use crate::query::CteScope;
use uqa_sql::{plan::SourcePlan, SQLError, SQLParam, ScalarExpr};

pub(super) fn reduce_null_rejected_outer_joins_to_fixpoint<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    source: &mut SourcePlan,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    uqa_sql::semantics::locking::null_rejection::reduce_null_rejected_outer_joins_to_fixpoint(
        context.catalog,
        source,
        predicate,
        params,
        &crate::query::binding::binding_context(ctes)?,
    )
}
