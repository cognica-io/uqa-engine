//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Floating-point input, arithmetic, and output at the declared SQL width.

use super::{division_by_zero, BinaryOp, Result, SQLError, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatWidth {
    Real,
    DoublePrecision,
}

pub(super) fn to_float(value: &Value, width: FloatWidth) -> Result<f64> {
    match value {
        Value::Float(value) => match width {
            FloatWidth::Real => narrow_real(*value).map(f64::from),
            FloatWidth::DoublePrecision => Ok(*value),
        },
        Value::Int(value) => Ok(match width {
            FloatWidth::Real => f64::from(*value as f32),
            FloatWidth::DoublePrecision => *value as f64,
        }),
        Value::Bool(value) => Ok(f64::from(u8::from(*value))),
        Value::Str(value) | Value::FixedChar(value) => parse_float(value, width),
        Value::Decimal(value) => parse_float(&value.to_sql_string(), width),
        other => Err(SQLError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

fn narrow_real(value: f64) -> Result<f32> {
    let narrowed = value as f32;
    if narrowed.is_infinite() && !value.is_infinite() {
        return Err(range_error("overflow"));
    }
    if narrowed == 0.0 && value != 0.0 {
        return Err(range_error("underflow"));
    }
    Ok(narrowed)
}

fn parse_float(input: &str, width: FloatWidth) -> Result<f64> {
    let text = input.trim_matches(|c: char| c.is_ascii_whitespace());
    let value = match width {
        FloatWidth::Real => text.parse::<f32>().map(f64::from),
        FloatWidth::DoublePrecision => text.parse::<f64>(),
    }
    .map_err(|_| SQLError::Routine {
        sqlstate: "22P02".into(),
        message: format!(
            "invalid input syntax for type {}: \"{input}\"",
            type_name(width)
        ),
    })?;
    let special = text
        .trim_start_matches(['+', '-'])
        .eq_ignore_ascii_case("inf")
        || text
            .trim_start_matches(['+', '-'])
            .eq_ignore_ascii_case("infinity");
    let mantissa = text.split(['e', 'E']).next().unwrap_or(text);
    let nonzero = mantissa.bytes().any(|byte| matches!(byte, b'1'..=b'9'));
    if (value.is_infinite() && !special) || (value == 0.0 && nonzero) {
        return Err(SQLError::Routine {
            sqlstate: "22003".into(),
            message: format!("\"{text}\" is out of range for type {}", type_name(width)),
        });
    }
    Ok(value)
}

fn type_name(width: FloatWidth) -> &'static str {
    match width {
        FloatWidth::Real => "real",
        FloatWidth::DoublePrecision => "double precision",
    }
}

fn range_error(kind: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22003".into(),
        message: format!("value out of range: {kind}"),
    }
}

/// Apply arithmetic at its resolved float width before widening the storage carrier.
pub fn eval_float_arithmetic(
    op: BinaryOp,
    left: &Value,
    right: &Value,
    width: FloatWidth,
) -> Result<Value> {
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(Value::Null);
    }
    let left = to_float(left, width)?;
    let right = to_float(right, width)?;
    if matches!(op, BinaryOp::Divide) && right == 0.0 && !left.is_nan() {
        return Err(division_by_zero());
    }
    let result = match width {
        FloatWidth::Real => {
            let left = left as f32;
            let right = right as f32;
            f64::from(match op {
                BinaryOp::Add => left + right,
                BinaryOp::Subtract => left - right,
                BinaryOp::Multiply => left * right,
                BinaryOp::Divide => left / right,
                _ => return Err(non_arithmetic(op)),
            })
        }
        FloatWidth::DoublePrecision => match op {
            BinaryOp::Add => left + right,
            BinaryOp::Subtract => left - right,
            BinaryOp::Multiply => left * right,
            BinaryOp::Divide => left / right,
            _ => return Err(non_arithmetic(op)),
        },
    };
    if result.is_infinite() && !left.is_infinite() && !right.is_infinite() {
        return Err(range_error("overflow"));
    }
    if result == 0.0
        && left != 0.0
        && match op {
            BinaryOp::Multiply => right != 0.0,
            BinaryOp::Divide => !right.is_infinite(),
            _ => false,
        }
    {
        return Err(range_error("underflow"));
    }
    Ok(Value::Float(result))
}

fn non_arithmetic(op: BinaryOp) -> SQLError {
    SQLError::Internal(format!(
        "non-arithmetic operator {op:?} reached floating arithmetic"
    ))
}

/// Format a real value using `PostgreSQL`'s shortest decimal and exponent thresholds.
#[must_use]
pub fn format_real(value: f32) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .into();
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("scientific float output");
    let exponent: i32 = exponent.parse().expect("scientific exponent");
    if (-4..6).contains(&exponent) {
        return value.to_string();
    }
    let sign = if exponent >= 0 { '+' } else { '-' };
    format!("{mantissa}e{sign}{:02}", exponent.abs())
}
