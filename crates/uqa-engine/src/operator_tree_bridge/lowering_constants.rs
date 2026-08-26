//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constant and named-argument decoding at the SQL/IR boundary.

use super::{
    eval_scalar, GatingSpec, OptionalStringConstant, SQLParam, ScalarEvalContext, ScalarExpr, Value,
};

pub(super) fn const_value(expr: &ScalarExpr, params: &[SQLParam]) -> Option<Value> {
    let ctx = ScalarEvalContext::new(None, params);
    eval_scalar(expr, &ctx).ok()
}

pub(super) fn const_string(expr: &ScalarExpr, params: &[SQLParam]) -> Option<String> {
    match const_value(expr, params)? {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

pub(super) fn const_optional_string(
    expr: &ScalarExpr,
    params: &[SQLParam],
) -> Option<OptionalStringConstant> {
    match const_value(expr, params)? {
        Value::Null => Some(OptionalStringConstant::Null),
        Value::Str(value) => Some(OptionalStringConstant::Value(value)),
        _ => None,
    }
}

pub(super) fn const_temporal_bound(
    expr: &ScalarExpr,
    params: &[SQLParam],
    null_default: f64,
) -> Option<f64> {
    match const_value(expr, params)? {
        Value::Null => Some(null_default),
        Value::Int(value) => Some(value as f64),
        Value::Float(value) => Some(value),
        Value::Decimal(value) => value.to_f64(),
        _ => None,
    }
}

pub(super) fn const_f64(expr: &ScalarExpr, params: &[SQLParam]) -> Option<f64> {
    match const_value(expr, params)? {
        Value::Int(n) => Some(n as f64),
        Value::Float(f) => Some(f),
        Value::Decimal(d) => d.to_f64(),
        _ => None,
    }
}

pub(super) fn const_bool(expr: &ScalarExpr, params: &[SQLParam]) -> Option<bool> {
    match const_value(expr, params)? {
        Value::Bool(value) => Some(value),
        _ => None,
    }
}

pub(super) fn const_usize(expr: &ScalarExpr, params: &[SQLParam]) -> Option<usize> {
    match const_value(expr, params)? {
        Value::Int(n) if n >= 0 => usize::try_from(n).ok(),
        _ => None,
    }
}

pub(super) fn const_vector(expr: &ScalarExpr, params: &[SQLParam]) -> Option<Vec<f32>> {
    match expr {
        ScalarExpr::Array(items) => {
            let mut out: Vec<f32> = Vec::with_capacity(items.len());
            for v in items {
                out.push(const_f64(v, params)? as f32);
            }
            Some(out)
        }
        other => match const_value(other, params)? {
            Value::Array(array) if array.dimensions().len() <= 1 => {
                let mut out: Vec<f32> = Vec::with_capacity(array.elements().len());
                for value in array.elements() {
                    match value {
                        Value::Int(number) => out.push(*number as f32),
                        Value::Float(number) => out.push(*number as f32),
                        Value::Decimal(number) => out.push(number.to_f64()? as f32),
                        _ => return None,
                    }
                }
                Some(out)
            }
            Value::List(items) => {
                let mut out: Vec<f32> = Vec::with_capacity(items.len());
                for v in items {
                    match v {
                        Value::Int(n) => out.push(n as f32),
                        Value::Float(f) => out.push(f as f32),
                        Value::Decimal(d) => out.push(d.to_f64()? as f32),
                        _ => return None,
                    }
                }
                Some(out)
            }
            _ => None,
        },
    }
}

pub(super) fn const_f64_vector(expr: &ScalarExpr, params: &[SQLParam]) -> Option<Vec<f64>> {
    match expr {
        ScalarExpr::Array(items) => items.iter().map(|value| const_f64(value, params)).collect(),
        other => match const_value(other, params)? {
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

pub(super) fn const_gating(expr: &ScalarExpr, params: &[SQLParam]) -> Option<GatingSpec> {
    match const_value(expr, params)? {
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
    let argument = uqa_execution::scalar_call_argument(expr).ok()?;
    Some((argument.name?, argument.value))
}
