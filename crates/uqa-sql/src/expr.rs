//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar expression evaluator: turns an [`Expr`] into a [`Value`] under
//! a row context (column -> value) and a parameter binding.

use uqa_core::Value;

use crate::ast::Expr;
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
            Ok(row.get(name).cloned().unwrap_or(Value::Null))
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
