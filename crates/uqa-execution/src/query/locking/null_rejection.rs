//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply immutable statement binding inputs for SQL outer-join null rejection.

use super::RowLockContext;
use crate::query::CteScope;
use uqa_core::memory::ProductionControl;
use uqa_sql::{plan::MutableSourcePlan, SQLError, SQLParam, ScalarExpr};

pub(super) fn reduce_null_rejected_outer_joins<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    source: MutableSourcePlan,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    source.reduce_null_rejected_outer_joins(
        context.catalog,
        predicate,
        params,
        &crate::query::binding::binding_context_with_control(ctes, control)?,
        control,
    )
}
