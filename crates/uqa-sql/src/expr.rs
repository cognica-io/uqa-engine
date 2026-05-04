//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar expression evaluator: turns an [`Expr`] into a [`Value`] under
//! a row context (column -> value) and a parameter binding.

use uqa_core::Value;

use crate::ast::{BinaryOp, Expr};
use crate::error::{Result, SqlError};
use crate::params::SqlParam;
use crate::result::ResultRow;

pub struct EvalContext<'a> {
    pub row: Option<&'a ResultRow>,
    pub params: &'a [SqlParam],
}

impl<'a> EvalContext<'a> {
    pub fn new(row: Option<&'a ResultRow>, params: &'a [SqlParam]) -> Self {
        Self { row, params }
    }
}

/// Evaluate a value-producing expression. Function calls are *not*
/// dispatched here; the compiler routes them through the function
/// registry instead. Calling `eval` on a `Func` expr returns
/// `Unsupported` so latent function-in-projection bugs surface loudly.
pub fn eval(expr: &Expr, ctx: &EvalContext<'_>) -> Result<Value> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Param(i) => match ctx.params.get(i.saturating_sub(1)) {
            Some(SqlParam::Scalar(v)) => Ok(v.clone()),
            Some(SqlParam::Vector(v)) => Ok(Value::List(
                v.iter().map(|x| Value::Float(f64::from(*x))).collect(),
            )),
            None => Err(SqlError::MissingParam(*i)),
        },
        Expr::Column(name) => {
            let row = ctx
                .row
                .ok_or_else(|| SqlError::Internal("column reference without row context".into()))?;
            // Plain column refs match either an unqualified key or the
            // suffix of a qualified `table.col` key, so the same row
            // shape works for single-table SELECTs and JOIN tuples.
            if let Some(v) = row.get(name) {
                return Ok(v.clone());
            }
            let suffix = format!(".{name}");
            for (key, value) in row {
                if key.ends_with(&suffix) {
                    return Ok(value.clone());
                }
            }
            Ok(Value::Null)
        }
        Expr::QualifiedColumn { qualifier, column } => {
            let row = ctx
                .row
                .ok_or_else(|| SqlError::Internal("column reference without row context".into()))?;
            let key = format!("{qualifier}.{column}");
            Ok(row.get(&key).cloned().unwrap_or(Value::Null))
        }
        Expr::Array(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            for e in elements {
                out.push(eval(e, ctx)?);
            }
            Ok(Value::List(out))
        }
        Expr::Star => Err(SqlError::Internal("`*` cannot be evaluated".into())),
        Expr::Func { name, .. } => Err(SqlError::Unsupported(format!(
            "scalar evaluation of `{name}` is not supported (use the function registry)"
        ))),
        Expr::Binary { op, lhs, rhs } => eval_binary(*op, lhs, rhs, ctx),
        Expr::Not(inner) => {
            let v = eval(inner, ctx)?;
            Ok(Value::Bool(!truthy(&v)))
        }
        Expr::And(items) => {
            for item in items {
                let v = eval(item, ctx)?;
                if !truthy(&v) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        Expr::Or(items) => {
            for item in items {
                let v = eval(item, ctx)?;
                if truthy(&v) {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        Expr::IsNull { expr, negated } => {
            let v = eval(expr, ctx)?;
            let is_null = matches!(v, Value::Null);
            Ok(Value::Bool(if *negated { !is_null } else { is_null }))
        }
        Expr::Between { expr, low, high } => {
            let v = eval(expr, ctx)?;
            let lo = eval(low, ctx)?;
            let hi = eval(high, ctx)?;
            let ge = compare(&v, &lo)?.is_ge();
            let le = compare(&v, &hi)?.is_le();
            Ok(Value::Bool(ge && le))
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            let v = eval(expr, ctx)?;
            for item in list {
                let candidate = eval(item, ctx)?;
                if values_equal(&v, &candidate) {
                    return Ok(Value::Bool(!*negated));
                }
            }
            Ok(Value::Bool(*negated))
        }
    }
}

fn eval_binary(op: BinaryOp, lhs: &Expr, rhs: &Expr, ctx: &EvalContext<'_>) -> Result<Value> {
    let l = eval(lhs, ctx)?;
    let r = eval(rhs, ctx)?;
    match op {
        BinaryOp::Equal => Ok(Value::Bool(values_equal(&l, &r))),
        BinaryOp::NotEqual => Ok(Value::Bool(!values_equal(&l, &r))),
        BinaryOp::Less => Ok(Value::Bool(compare(&l, &r)?.is_lt())),
        BinaryOp::LessEqual => Ok(Value::Bool(compare(&l, &r)?.is_le())),
        BinaryOp::Greater => Ok(Value::Bool(compare(&l, &r)?.is_gt())),
        BinaryOp::GreaterEqual => Ok(Value::Bool(compare(&l, &r)?.is_ge())),
        BinaryOp::Add => arith(&l, &r, op),
        BinaryOp::Subtract => arith(&l, &r, op),
        BinaryOp::Multiply => arith(&l, &r, op),
        BinaryOp::Divide => arith(&l, &r, op),
    }
}

/// `NULL` is falsy; otherwise truthy iff the value coerces to a non-zero
/// boolean / number / non-empty string.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        _ => true,
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        _ => a == b,
    }
}

fn compare(a: &Value, b: &Value) -> Result<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => Ok(Ordering::Equal),
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => Ok(x.partial_cmp(y).unwrap_or(Ordering::Equal)),
        (Value::Int(x), Value::Float(y)) => {
            Ok((*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal))
        }
        (Value::Float(x), Value::Int(y)) => {
            Ok(x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal))
        }
        (Value::Str(x), Value::Str(y)) => Ok(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        (lhs, rhs) => Err(SqlError::TypeMismatch(format!(
            "cannot compare {lhs:?} with {rhs:?}"
        ))),
    }
}

fn arith(a: &Value, b: &Value, op: BinaryOp) -> Result<Value> {
    let lf = to_f64(a)?;
    let rf = to_f64(b)?;
    let result = match op {
        BinaryOp::Add => lf + rf,
        BinaryOp::Subtract => lf - rf,
        BinaryOp::Multiply => lf * rf,
        BinaryOp::Divide => {
            if rf == 0.0 {
                return Err(SqlError::TypeMismatch("division by zero".into()));
            }
            lf / rf
        }
        _ => unreachable!("non-arith op routed through arith"),
    };
    // Preserve integer-ness when both operands were ints and the result
    // is whole.
    if matches!((a, b), (Value::Int(_), Value::Int(_))) && result.fract() == 0.0 {
        Ok(Value::Int(result as i64))
    } else {
        Ok(Value::Float(result))
    }
}

fn to_f64(v: &Value) -> Result<f64> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(SqlError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

/// Coerce a [`Value`] into a `Vec<f32>` if it is a homogeneous numeric
/// list (used to read vector literals from `ARRAY[...]` or `$N` Vector
/// params).
pub fn value_to_vector(v: &Value) -> Result<Vec<f32>> {
    match v {
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let x = match item {
                    Value::Float(f) => *f as f32,
                    Value::Int(i) => *i as f32,
                    other => {
                        return Err(SqlError::TypeMismatch(format!(
                            "vector element must be numeric, got {other:?}"
                        )))
                    }
                };
                out.push(x);
            }
            Ok(out)
        }
        other => Err(SqlError::TypeMismatch(format!(
            "expected vector (numeric list), got {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Expr;

    #[test]
    fn literal_passthrough() {
        let ctx = EvalContext::new(None, &[]);
        let got = eval(&Expr::Literal(Value::Int(42)), &ctx).unwrap();
        assert_eq!(got, Value::Int(42));
    }

    #[test]
    fn param_scalar_returns_value() {
        let params = vec![SqlParam::Scalar(Value::Str("hi".into()))];
        let ctx = EvalContext::new(None, &params);
        let got = eval(&Expr::Param(1), &ctx).unwrap();
        assert_eq!(got, Value::Str("hi".into()));
    }

    #[test]
    fn array_collects_into_list() {
        let ctx = EvalContext::new(None, &[]);
        let got = eval(
            &Expr::Array(vec![
                Expr::Literal(Value::Int(1)),
                Expr::Literal(Value::Int(2)),
            ]),
            &ctx,
        )
        .unwrap();
        assert_eq!(got, Value::List(vec![Value::Int(1), Value::Int(2)]));
    }

    #[test]
    fn value_to_vector_accepts_floats_and_ints() {
        let v = Value::List(vec![Value::Float(0.5), Value::Int(1), Value::Float(-1.5)]);
        let got = value_to_vector(&v).unwrap();
        assert_eq!(got, vec![0.5, 1.0, -1.5]);
    }
}
