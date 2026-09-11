//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL argument coercion and diagnostics for native and AGE graph commands.

use crate::{semantics::retrieval::expect_evaluated_string, SQLError, ScalarExpr};
use uqa_core::Value;

pub const AGE_INVALID_PARAMETER_VALUE: &str = "22023";
pub const AGE_UNDEFINED_SCHEMA: &str = "3F000";
pub const AGE_DUPLICATE_SCHEMA: &str = "42P06";
pub const AGE_UNDEFINED_TABLE: &str = "42P01";
pub const AGE_FEATURE_NOT_SUPPORTED: &str = "0A000";
pub const AGE_DEPENDENT_OBJECTS_STILL_EXIST: &str = "2BP01";

pub fn age_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.to_string(),
        message: message.into(),
    }
}

/// Evaluate a `name`/`cstring` argument of an AGE management function.
/// `null_message` is the AGE error for a SQL NULL argument.
pub fn eval_age_name_with(
    expr: &ScalarExpr,
    null_message: &str,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<String, SQLError> {
    match evaluate(expr)? {
        Value::Null => Err(age_error(AGE_INVALID_PARAMETER_VALUE, null_message)),
        Value::Str(s) | Value::FixedChar(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "graph name must be a string, got {other:?}"
        ))),
    }
}

pub fn eval_age_graph_name_with(
    expr: &ScalarExpr,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<String, SQLError> {
    eval_age_name_with(expr, "graph name can not be NULL", evaluate)
}

pub fn eval_age_bool_with(
    expr: &ScalarExpr,
    argument: &str,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<bool, SQLError> {
    match evaluate(expr)? {
        Value::Bool(value) => Ok(value),
        other => Err(SQLError::TypeMismatch(format!(
            "{argument} must be a boolean, got {other:?}"
        ))),
    }
}

pub fn require_age_arity(
    name: &str,
    args: &[ScalarExpr],
    range: std::ops::RangeInclusive<usize>,
) -> Result<(), SQLError> {
    if range.contains(&args.len()) {
        return Ok(());
    }
    let expected = if range.start() == range.end() {
        range.start().to_string()
    } else {
        format!("{} or {}", range.start(), range.end())
    };
    Err(SQLError::BadArity {
        name: name.into(),
        expected,
        actual: args.len(),
    })
}

pub fn graph_create_name(
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<String, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "graph_create".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    expect_evaluated_string(evaluate(&args[0])?, "graph_create.name")
}

pub fn graph_drop_name(
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<String, SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: "graph_drop".into(),
            expected: "1 or 2".into(),
            actual: args.len(),
        });
    }
    expect_evaluated_string(evaluate(&args[0])?, "graph_drop.name")
}

/// Evaluate the optional cascade argument after the caller reads graph existence.
pub fn validate_graph_drop_cascade(
    name: &str,
    graph_exists: bool,
    cascade_expr: Option<&ScalarExpr>,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<(), SQLError> {
    if let Some(cascade_expr) = cascade_expr {
        match evaluate(cascade_expr)? {
            Value::Bool(false) if graph_exists => {
                return Err(SQLError::Unsupported(format!(
                    "cannot drop graph {name:?} without cascade"
                )));
            }
            Value::Bool(_) => {}
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "graph_drop.cascade must be a boolean, got {other:?}"
                )));
            }
        }
    }
    Ok(())
}
