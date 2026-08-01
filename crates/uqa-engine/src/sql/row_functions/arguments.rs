//! Shared evaluated argument and column-name extraction.

use super::{eval_scalar, SQLError, ScalarEvalContext, ScalarExpr, Value};

pub(super) fn expect_string(
    expr: &ScalarExpr,
    name: &str,
    ctx: &ScalarEvalContext,
) -> Result<String, SQLError> {
    expect_evaluated_string(eval_scalar(expr, ctx)?, name)
}

pub(super) fn expect_evaluated_string(value: Value, name: &str) -> Result<String, SQLError> {
    match value {
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must be a string, got {other:?}"
        ))),
    }
}

pub(in crate::sql) fn expect_column_name(
    expr: &ScalarExpr,
    label: &str,
) -> Result<String, SQLError> {
    match expr {
        ScalarExpr::Column(name) => Ok(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Ok(column.clone()),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be a column reference, got {other:?}"
        ))),
    }
}

pub(super) fn expect_field_name_or_string(
    expr: &ScalarExpr,
    label: &str,
    ctx: &ScalarEvalContext<'_>,
) -> Result<String, SQLError> {
    match expr {
        ScalarExpr::Column(name) => Ok(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Ok(column.clone()),
        _ => expect_string(expr, label, ctx),
    }
}

pub(super) fn expect_usize(
    expr: &ScalarExpr,
    label: &str,
    ctx: &ScalarEvalContext<'_>,
) -> Result<usize, SQLError> {
    let v = eval_scalar(expr, ctx)?;
    match v {
        Value::Int(n) if n >= 0 => usize::try_from(n).map_err(|_| {
            SQLError::TypeMismatch(format!("{label} exceeds the platform usize range"))
        }),
        Value::Int(_) => Err(SQLError::TypeMismatch(format!("{label} must be >= 0"))),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be an integer, got {other:?}"
        ))),
    }
}
