//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constant and named-argument decoding at the SQL/IR boundary.

use super::{GatingSpec, OptionalStringConstant, RetrievalConstants, ScalarExpr, Value};

pub(super) fn const_value(expr: &ScalarExpr, constants: &RetrievalConstants<'_>) -> Option<Value> {
    (constants.evaluate)(expr, constants.params).ok()
}

pub(super) fn const_string(
    expr: &ScalarExpr,
    constants: &RetrievalConstants<'_>,
) -> Option<String> {
    match const_value(expr, constants)? {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

pub(super) fn const_optional_string(
    expr: &ScalarExpr,
    constants: &RetrievalConstants<'_>,
) -> Option<OptionalStringConstant> {
    match const_value(expr, constants)? {
        Value::Null => Some(OptionalStringConstant::Null),
        Value::Str(value) => Some(OptionalStringConstant::Value(value)),
        _ => None,
    }
}

pub(super) fn const_temporal_bound(
    expr: &ScalarExpr,
    constants: &RetrievalConstants<'_>,
    null_default: f64,
) -> Option<f64> {
    match const_value(expr, constants)? {
        Value::Null => Some(null_default),
        Value::Int(value) => Some(value as f64),
        Value::Float(value) => Some(value),
        Value::Decimal(value) => value.to_f64(),
        _ => None,
    }
}

pub(super) fn const_f64(expr: &ScalarExpr, constants: &RetrievalConstants<'_>) -> Option<f64> {
    match const_value(expr, constants)? {
        Value::Int(n) => Some(n as f64),
        Value::Float(f) => Some(f),
        Value::Decimal(d) => d.to_f64(),
        _ => None,
    }
}

pub(super) fn const_bool(expr: &ScalarExpr, constants: &RetrievalConstants<'_>) -> Option<bool> {
    match const_value(expr, constants)? {
        Value::Bool(value) => Some(value),
        _ => None,
    }
}

pub(super) fn const_usize(expr: &ScalarExpr, constants: &RetrievalConstants<'_>) -> Option<usize> {
    match const_value(expr, constants)? {
        Value::Int(n) if n >= 0 => usize::try_from(n).ok(),
        _ => None,
    }
}

pub(super) fn const_f64_vector(
    expr: &ScalarExpr,
    constants: &RetrievalConstants<'_>,
) -> Option<Vec<f64>> {
    match expr {
        ScalarExpr::Array(items) => items
            .iter()
            .map(|value| const_f64(value, constants))
            .collect(),
        other => match const_value(other, constants)? {
            Value::Array(array) if array.dimensions().len() <= 1 => array
                .elements()
                .iter()
                .map(|value| match value {
                    Value::Int(number) => Some(*number as f64),
                    Value::Float(number) => Some(*number),
                    Value::Decimal(number) => number.to_f64(),
                    _ => None,
                })
                .collect(),
            Value::List(items) => items
                .into_iter()
                .map(|value| match value {
                    Value::Int(number) => Some(number as f64),
                    Value::Float(number) => Some(number),
                    Value::Decimal(number) => number.to_f64(),
                    _ => None,
                })
                .collect(),
            _ => None,
        },
    }
}

pub(super) fn const_gating(
    expr: &ScalarExpr,
    constants: &RetrievalConstants<'_>,
) -> Option<GatingSpec> {
    match const_value(expr, constants)? {
        Value::Str(s) if s.eq_ignore_ascii_case("softplus") => Some(GatingSpec::Softplus),
        Value::Str(s) if s.eq_ignore_ascii_case("pass") || s.eq_ignore_ascii_case("none") => {
            Some(GatingSpec::Pass)
        }
        Value::Str(s) if s.eq_ignore_ascii_case("sigmoid") => Some(GatingSpec::Sigmoid {
            feature: String::new(),
        }),
        Value::Str(s) if s.eq_ignore_ascii_case("relu") => Some(GatingSpec::ReLU),
        Value::Str(s) if s.eq_ignore_ascii_case("swish") => Some(GatingSpec::Swish),
        Value::Str(s) if s.eq_ignore_ascii_case("gelu") => Some(GatingSpec::Gelu),
        _ => None,
    }
}

pub(super) fn named_arg_expr(expr: &ScalarExpr) -> Option<(&str, &ScalarExpr)> {
    let argument = crate::scalar_call_argument(expr).ok()?;
    Some((argument.name?, argument.value))
}
