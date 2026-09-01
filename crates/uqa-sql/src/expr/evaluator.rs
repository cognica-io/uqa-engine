//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! AST scalar evaluation orchestration.

use uqa_core::{ArrayValue, Value};

use crate::ast::{Expr, FunctionResolutionError};
use crate::error::{Result, SQLError};
use crate::params::SQLParam;

use super::binary::{compare_nullable, eval_binary, truthy, values_equal, values_equal_nullable};
use super::builtin::eval_bound_builtin_function_call;
use super::call_arguments::evaluate_call_args;
use super::call_dispatch::eval_function_call;
use super::casting::negate_value;
use super::context::{cast_value_with_type_resolution, EvalContext};

/// Evaluate a value-producing AST expression against one row and parameter context.
pub fn eval(expr: &Expr, ctx: &EvalContext<'_>) -> Result<Value> {
    match expr {
        Expr::Default => Err(SQLError::Internal(
            "DEFAULT reached scalar expression evaluation without a mutation target".into(),
        )),
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Param(i) => match i.checked_sub(1).and_then(|index| ctx.params.get(index)) {
            Some(SQLParam::Scalar(v) | SQLParam::TypedScalar { value: v, .. }) => Ok(v.clone()),
            Some(SQLParam::Vector(v)) => Ok(Value::List(
                v.iter().map(|x| Value::Float(f64::from(*x))).collect(),
            )),
            Some(SQLParam::Tensor(vectors)) => Ok(Value::List(
                vectors
                    .iter()
                    .map(|vector| {
                        Value::List(vector.iter().map(|x| Value::Float(f64::from(*x))).collect())
                    })
                    .collect(),
            )),
            None => Err(SQLError::MissingParam(*i)),
        },
        Expr::Column(name) => {
            // Plain column refs match either an unqualified key or the
            // suffix of a qualified `table.col` key, so the same row
            // shape works for single-table SELECTs and JOIN tuples.
            if ctx.row_lookup()?.column_is_ambiguous(name) {
                return Err(SQLError::AmbiguousColumn(name.clone()));
            }
            Ok(ctx
                .row_lookup()?
                .column(name)
                .cloned()
                .unwrap_or(Value::Null))
        }
        Expr::QualifiedColumn { qualifier, column } => {
            if ctx
                .row_lookup()?
                .qualified_column_is_ambiguous(qualifier, column)
            {
                return Err(SQLError::AmbiguousColumn(format!("{qualifier}.{column}")));
            }
            Ok(ctx
                .row_lookup()?
                .qualified_column(qualifier, column)
                .cloned()
                .unwrap_or(Value::Null))
        }
        Expr::InternalColumn(column) => ctx
            .row_lookup()?
            .internal_column(*column)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "internal relation attribute {column:?} is unavailable"
                ))
            }),
        Expr::Array(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            for e in elements {
                out.push(eval(e, ctx)?);
            }
            ArrayValue::try_new(out).map(Value::Array).ok_or_else(|| {
                SQLError::TypeMismatch(
                    "multidimensional arrays must have matching dimensions".into(),
                )
            })
        }
        Expr::Row(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            for element in elements {
                out.push(eval(element, ctx)?);
            }
            Ok(Value::Row(out))
        }
        Expr::Star | Expr::QualifiedStar(_) => {
            Err(SQLError::Internal("`*` cannot be evaluated".into()))
        }
        Expr::Func {
            name,
            binding,
            args,
            ..
        } => {
            let call_args = evaluate_call_args(args, ctx)?;
            if let Some(binding) = binding {
                if let Some(FunctionResolutionError::UndefinedFunction { signature }) =
                    binding.resolution_error.as_ref()
                {
                    return Err(SQLError::Routine {
                        sqlstate: "42883".into(),
                        message: format!("function {signature} does not exist"),
                    });
                }
                if binding.builtin {
                    return eval_bound_builtin_function_call(binding, call_args, ctx);
                }
                let engine = ctx.engine.ok_or_else(|| {
                    SQLError::Unsupported(
                        "bound user function requires a logical engine session".into(),
                    )
                })?;
                engine
                    .call_bound_user_function(binding, &call_args)
                    .unwrap_or_else(|| Err(SQLError::UnknownFunction(binding.name.clone())))
            } else {
                eval_function_call(name, call_args, ctx)
            }
        }
        Expr::WindowCall { name, .. } => Err(SQLError::Unsupported(format!(
            "window function `{name}` must be evaluated by the window-aware executor"
        ))),
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let base_value = match base {
                Some(b) => Some(eval(b, ctx)?),
                None => None,
            };
            for (cond, result) in when {
                let matched = match &base_value {
                    Some(bv) => values_equal(bv, &eval(cond, ctx)?),
                    None => truthy(&eval(cond, ctx)?),
                };
                if matched {
                    return eval(result, ctx);
                }
            }
            match else_branch {
                Some(e) => eval(e, ctx),
                None => Ok(Value::Null),
            }
        }
        Expr::Cast { expr, ty } => {
            let source_ty = explicit_expr_type(expr);
            let v = eval(expr, ctx)?;
            cast_value_with_type_resolution(&v, source_ty, ty, ctx.engine)
        }
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
            Err(SQLError::Unsupported(
                "query-valued expressions must be lowered to physical ScalarExpr/QueryPlan slots"
                    .into(),
            ))
        }
        Expr::Binary { op, lhs, rhs } => eval_binary(*op, lhs, rhs, ctx),
        Expr::UnaryMinus(inner) => {
            let source_ty = explicit_expr_type(inner);
            let value = eval(inner, ctx)?;
            negate_value(&value, source_ty)
        }
        Expr::Not(inner) => {
            // SQL three-valued logic: NOT NULL -> NULL.
            let v = eval(inner, ctx)?;
            if matches!(v, Value::Null) {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(!truthy(&v)))
        }
        Expr::And(items) => {
            // Kleene AND: FALSE dominates, otherwise NULL taints.
            let mut saw_null = false;
            for item in items {
                let v = eval(item, ctx)?;
                if matches!(v, Value::Null) {
                    saw_null = true;
                } else if !truthy(&v) {
                    return Ok(Value::Bool(false));
                }
            }
            if saw_null {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(true))
        }
        Expr::Or(items) => {
            // Kleene OR: TRUE dominates, otherwise NULL taints.
            let mut saw_null = false;
            for item in items {
                let v = eval(item, ctx)?;
                if matches!(v, Value::Null) {
                    saw_null = true;
                } else if truthy(&v) {
                    return Ok(Value::Bool(true));
                }
            }
            if saw_null {
                return Ok(Value::Null);
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
            eval_between(&v, &lo, &hi)
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            // Three-valued IN: found -> TRUE, a NULL comparand (or a
            // NULL needle) downgrades a miss to NULL.
            let v = eval(expr, ctx)?;
            let mut saw_null = matches!(v, Value::Null);
            for item in list {
                let candidate = eval(item, ctx)?;
                match values_equal_nullable(&v, &candidate) {
                    Some(true) => return Ok(Value::Bool(!*negated)),
                    Some(false) => {}
                    None => saw_null = true,
                }
            }
            if saw_null {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(*negated))
        }
    }
}

fn explicit_expr_type(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Cast { ty, .. } => Some(ty),
        Expr::Literal(Value::Int(value)) if i32::try_from(*value).is_ok() => Some("integer"),
        Expr::Literal(Value::Int(_)) => Some("bigint"),
        Expr::Literal(Value::Bytes(_)) => Some("bytea"),
        _ => None,
    }
}

/// `expr BETWEEN low AND high` under three-valued logic: a definite
/// FALSE on either bound wins over a NULL on the other.
pub(super) fn eval_between(v: &Value, lo: &Value, hi: &Value) -> Result<Value> {
    let ge = compare_nullable(v, lo)?.map(|ord| ord.is_ge());
    let le = compare_nullable(v, hi)?.map(|ord| ord.is_le());
    Ok(match (ge, le) {
        (Some(false), _) | (_, Some(false)) => Value::Bool(false),
        (Some(true), Some(true)) => Value::Bool(true),
        _ => Value::Null,
    })
}
