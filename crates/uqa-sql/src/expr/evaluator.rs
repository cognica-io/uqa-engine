//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! AST scalar evaluation orchestration.

use uqa_core::Value;

use crate::ast::{BinaryOp, Expr};
use crate::error::{Result, SQLError};

use super::binary::{eval_binary, eval_comparison_truth_with_engine, truthy};
use super::builtin::eval_bound_builtin_function_call;
use super::call_arguments::evaluate_call_args;
use super::call_dispatch::eval_function_call;
use super::casting::negate_value;
use super::context::{cast_value_with_type_resolution, EvalContext};

/// Evaluate a value-producing AST expression against one row and parameter context.
#[expect(
    clippy::too_many_lines,
    reason = "builtin dispatch preserves arity, NULL, and error precedence"
)]
pub fn eval(expr: &Expr, ctx: &EvalContext<'_>) -> Result<Value> {
    match expr {
        Expr::Default => Err(SQLError::Internal(
            "DEFAULT reached scalar expression evaluation without a mutation target".into(),
        )),
        Expr::Literal(v) => Ok(v.clone()),
        Expr::RetainedLiteral(literal) => eval(&literal.materialize()?, ctx),
        Expr::TypedLiteral { value, ty, .. } => {
            Ok(super::composites::literal::evaluate_with_control(
                value,
                ty,
                ctx.engine,
                &uqa_core::memory::ProductionControl::uncontrolled(),
            )?
            .into_uncontrolled()
            .expect("ordinary constant result"))
        }
        Expr::Param(i) => match i.checked_sub(1).and_then(|index| ctx.params.get(index)) {
            Some(parameter) => parameter.to_value(),
            None => Err(SQLError::MissingParam(*i)),
        },
        Expr::Column(name) => {
            // Plain column refs match either an unqualified key or the
            // suffix of a qualified `table.col` key, so the same row
            // shape works for single-table SELECTs and JOIN tuples.
            if ctx.row_lookup()?.column_is_ambiguous(name) {
                return Err(SQLError::AmbiguousColumn(name.to_string()));
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
            let scalar = crate::plan::ExpressionPlan::lower(Expr::Array(elements.clone())).scalar;
            let crate::ScalarExpr::Array(items) = scalar else {
                unreachable!("array lowering")
            };
            let control = uqa_core::memory::ProductionControl::uncontrolled();
            let element_type = crate::type_resolution::scalar_array_input_type_with_control(
                &items,
                &crate::RowSchema::default(),
                ctx.params,
                ctx.engine,
                &control,
            )?;
            Ok(super::construct_array_with_control(
                elements.len(),
                element_type.as_deref(),
                &control,
                |index| Ok(control.finish(eval(&elements[index], ctx)?, None)?),
            )?
            .into_uncontrolled()
            .expect("ordinary array constructor"))
        }
        Expr::CompositeRow { items, binding } => {
            let control = uqa_core::memory::ProductionControl::uncontrolled();
            Ok(super::composites::constructor::evaluate_with_control(
                binding,
                items.len(),
                ctx.engine,
                &control,
                |index| {
                    control
                        .finish(eval(&items[index], ctx)?, None)
                        .map_err(Into::into)
                },
            )?
            .into_uncontrolled()
            .expect("ordinary composite constructor"))
        }
        Expr::Row(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            let mut fields = Vec::with_capacity(elements.len());
            for element in elements {
                let scalar = crate::plan::ExpressionPlan::lower(element.clone()).scalar;
                if let Some(field) = crate::type_resolution::scalar_record_field_type_with_control(
                    &scalar,
                    &crate::RowSchema::default(),
                    ctx.params,
                    ctx.engine,
                    &uqa_core::memory::ProductionControl::uncontrolled(),
                )? {
                    fields.push(field);
                }
                out.push(eval(element, ctx)?);
            }
            let row = if fields.len() == out.len() {
                uqa_core::RowValue::typed(out, fields)?
            } else {
                uqa_core::RowValue::new(out)
            };
            Ok(Value::Row(row))
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
            if let Some(binding) = binding {
                if let Some(error) = &binding.resolution_error {
                    return Err(error.sql_error());
                }
                if let Some(clause) = crate::catalog::collations::expression::ast_clause(expr)? {
                    // Analysis retains the selected collation for consumers; evaluating the clause only evaluates its operand.
                    return eval(clause.argument, ctx);
                }
                if let Some(hook) = ctx.engine {
                    hook.require_builtin_execute(binding)?;
                }
                if let Some(dispatch) = binding.dispatch {
                    if let Some(result) =
                        super::eval_distinct_boolean(dispatch, args.len(), |index| {
                            eval(&args[index], ctx)
                        })
                    {
                        return result;
                    }
                }
                if let Some(crate::ast::FunctionDispatch::NumericOperator(operator)) =
                    binding.dispatch
                {
                    return super::numeric_operator::eval_ast_operator(
                        operator, binding, args, ctx,
                    );
                }
            }
            if name.eq_ignore_ascii_case("coalesce")
                && binding.as_ref().is_none_or(|binding| binding.builtin)
            {
                for argument in args {
                    let value = eval(argument, ctx)?;
                    if !matches!(value, Value::Null) {
                        return Ok(value);
                    }
                }
                return Ok(Value::Null);
            }
            if binding.as_ref().is_some_and(|binding| {
                binding.builtin
                    && binding.dispatch == Some(crate::ast::FunctionDispatch::BetweenSymmetric)
            }) {
                let [value, low, high] = args.as_slice() else {
                    return Err(SQLError::TypeMismatch(
                        "BETWEEN SYMMETRIC takes 3 args".into(),
                    ));
                };
                let forward = eval_between(value, low, high, ctx, 0)?;
                if forward == Value::Bool(true) {
                    return Ok(forward);
                }
                let backward = eval_between(value, high, low, ctx, 2)?;
                return Ok(match (forward, backward) {
                    (_, Value::Bool(true)) => Value::Bool(true),
                    (Value::Null, _) | (_, Value::Null) => Value::Null,
                    _ => Value::Bool(false),
                });
            }
            if let Some(access) = binding
                .as_ref()
                .and_then(|binding| binding.dispatch)
                .and_then(|dispatch| {
                    crate::ast::ArrayAccess::from_dispatch(dispatch, args.len(), |index| {
                        matches!(args[index], Expr::Literal(Value::Null))
                    })
                })
            {
                let control = uqa_core::memory::ProductionControl::uncontrolled();
                return Ok(
                    super::eval_array_access_with_control(access?, &control, |index| {
                        Ok(control.finish(eval(&args[index], ctx)?, None)?)
                    })?
                    .into_uncontrolled()
                    .expect("ordinary array access"),
                );
            }
            // A set-returning built-in cannot produce the one value this expression needs; `PostgreSQL` rejects it before evaluating its arguments.
            if let Some(binding) = binding.as_ref().filter(|binding| binding.builtin) {
                if crate::semantics::sets::static_setness::builtin_call_returns_set(
                    name,
                    Some(binding),
                ) {
                    return Err(crate::semantics::sets::placement::set_valued_call_error());
                }
            }
            let call_args = evaluate_call_args(args, ctx)?;
            if let Some(binding) = binding {
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
                    Some(bv) => {
                        super::eval_value_comparison_truth_with_engine(
                            BinaryOp::Equal,
                            bv,
                            &eval(cond, ctx)?,
                            &uqa_core::memory::ProductionControl::uncontrolled(),
                            ctx.engine,
                            ctx.enum_comparison_state_at(0),
                        )? == Some(true)
                    }
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
        Expr::Cast { expr, ty, .. } => {
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
            Ok(Value::Bool(uqa_core::sql_null_test(Some(&v), *negated)))
        }
        Expr::Between { expr, low, high } => eval_between(expr, low, high, ctx, 0),
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
                match compare(BinaryOp::Equal, &v, &candidate, ctx, 0)? {
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
        Expr::Cast { ty, .. }
        | Expr::TypedLiteral { ty, .. }
        | Expr::CompositeRow {
            binding: crate::ast::CompositeRowBinding { ty, .. },
            ..
        } => Some(ty),
        Expr::RetainedLiteral(literal) => literal.0.declaration.as_deref().or(match literal.0.ty {
            crate::ast::ColumnType::Bytea => Some("bytea"),
            crate::ast::ColumnType::Bit(_) => Some("bit"),
            _ => None,
        }),
        Expr::Literal(Value::Int(value)) if i32::try_from(*value).is_ok() => Some("integer"),
        Expr::Literal(Value::Int(_)) => Some("bigint"),
        Expr::Literal(Value::Bytes(_)) => Some("bytea"),
        Expr::Literal(Value::BitString(_)) => Some("bit"),
        _ => None,
    }
}

/// Evaluate `PostgreSQL`'s two comparisons in order, including repeated value
/// evaluation and the short circuit after a false lower-bound comparison.
fn eval_between(
    expression: &Expr,
    low: &Expr,
    high: &Expr,
    context: &EvalContext<'_>,
    slot: usize,
) -> Result<Value> {
    let ge = compare(
        BinaryOp::GreaterEqual,
        &eval(expression, context)?,
        &eval(low, context)?,
        context,
        slot,
    )?;
    if ge == Some(false) {
        return Ok(Value::Bool(false));
    }
    let le = compare(
        BinaryOp::LessEqual,
        &eval(expression, context)?,
        &eval(high, context)?,
        context,
        slot + 1,
    )?;
    Ok(match (ge, le) {
        (_, Some(false)) => Value::Bool(false),
        (Some(true), Some(true)) => Value::Bool(true),
        _ => Value::Null,
    })
}

fn compare(
    op: BinaryOp,
    left: &Value,
    right: &Value,
    context: &EvalContext<'_>,
    slot: usize,
) -> Result<Option<bool>> {
    eval_comparison_truth_with_engine(
        op,
        left,
        right,
        &uqa_core::memory::ProductionControl::uncontrolled(),
        context.engine,
        context.enum_comparison_state_at(slot),
    )
}
