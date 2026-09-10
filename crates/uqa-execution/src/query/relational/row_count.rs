//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate row-count clauses in their query scope.

use super::RelationalContext;
use crate::query::CteScope;
use crate::scalar::plan::{eval_physical_scalar, PhysicalEvalContext};
use uqa_core::Value;
use uqa_sql::{semantics::row_count::coerce_limit_offset, SQLError, SQLParam, ScalarExpr};

pub fn resolve_limit_offset_with_ctes<S: Clone + 'static>(
    expr: Option<&ScalarExpr>,
    context: RelationalContext<'_, S>,
    params: &[SQLParam],
    label: &str,
    ctes: &CteScope<S>,
) -> Result<Option<u64>, SQLError> {
    let Some(expr) = expr else {
        return Ok(None);
    };
    let value = evaluate_limit_offset_with_ctes(expr, context, params, ctes)?;
    coerce_limit_offset(value, expr, label)
}

/// Resolve the mandatory row count for `FETCH ... WITH TIES`. `PostgreSQL` uses `invalid_row_count_in_result_offset_clause` for both a NULL and a negative boundary, with a clause-specific diagnostic for NULL.
pub fn resolve_fetch_limit_with_ties<S: Clone + 'static>(
    expr: Option<&ScalarExpr>,
    context: RelationalContext<'_, S>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<u64, SQLError> {
    let Some(expr) = expr else {
        return Err(SQLError::Internal(
            "FETCH ... WITH TIES is missing its row-count expression".into(),
        ));
    };
    let value = evaluate_limit_offset_with_ctes(expr, context, params, ctes)?;
    match value {
        Value::Null => Err(SQLError::Routine {
            sqlstate: "2201W".into(),
            message: "row count cannot be null in FETCH FIRST ... WITH TIES clause".into(),
        }),
        value => coerce_limit_offset(value, expr, "LIMIT")?.ok_or_else(|| {
            SQLError::Internal("FETCH ... WITH TIES resolved without a row count".into())
        }),
    }
}

fn evaluate_limit_offset_with_ctes<S: Clone + 'static>(
    expr: &ScalarExpr,
    context: RelationalContext<'_, S>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Value, SQLError> {
    let hook = context.expression_scope(ctes.clone());
    let ctx = PhysicalEvalContext::new(None, params)
        .with_function_hook(hook.as_ref())
        .with_subquery_runner(hook.as_ref());
    eval_physical_scalar(expr, &ctes.scalar_subqueries, &ctx)
}
