//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct evaluation operations for the physical scalar IR.

use uqa_core::{ArrayValue, Value};
use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::{
    cast_value_with_type_resolution, eval_binary_values, eval_binary_values_with_integer_width,
    eval_bound_builtin_function_call, eval_function_call, integer_width_for_literal,
    integer_width_for_type, negate_value, truthy, IntegerWidth,
};
use uqa_sql::{SQLError, SQLParam};

use super::call_arguments::eval_call_arguments;
use super::context::ScalarEvalContext;
use super::{ScalarExpr, SubqueryId};

/// Evaluate the physical scalar tree directly. No parser expression is reconstructed at this boundary.
#[expect(
    clippy::too_many_lines,
    reason = "scalar evaluation keeps IR variants and callback errors exhaustive"
)]
pub fn eval_scalar(
    expression: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    match expression {
        ScalarExpr::Default => Err(SQLError::Internal(
            "DEFAULT reached scalar expression evaluation without a mutation target".into(),
        )),
        ScalarExpr::Star => Err(SQLError::Internal("`*` cannot be evaluated".into())),
        ScalarExpr::QualifiedStar(qualifier) => evaluate_qualified_whole_row(qualifier, context),
        ScalarExpr::Column(name) => {
            if context.row_schema().is_some_and(|schema| {
                !schema.has_unqualified_column(name)
                    && !schema.column_is_ambiguous(name)
                    && schema.has_qualifier(name)
            }) {
                evaluate_qualified_whole_row(name, context)
            } else {
                context.sql_context().column_value(name)
            }
        }
        ScalarExpr::Position(position) => context
            .row_lookup()
            .and_then(|row| row.positional_column(*position))
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "bound physical column position {position} is unavailable"
                ))
            }),
        ScalarExpr::InternalColumn(column) => context
            .row_lookup()
            .and_then(|row| row.internal_column(*column))
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "internal relation attribute {column:?} is unavailable"
                ))
            }),
        ScalarExpr::QualifiedColumn { qualifier, column } => context
            .sql_context()
            .qualified_column_value(qualifier, column),
        ScalarExpr::Literal(value) => Ok(value.clone()),
        ScalarExpr::Param(index) => eval_parameter(*index, context.params()),
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            let arguments = eval_call_arguments(args, context)?;
            if let Some(binding) = binding {
                if let Some(uqa_sql::ast::FunctionResolutionError::UndefinedFunction {
                    signature,
                }) = binding.resolution_error.as_ref()
                {
                    return Err(SQLError::Routine {
                        sqlstate: "42883".into(),
                        message: format!("function {signature} does not exist"),
                    });
                }
                if binding.builtin {
                    if let Some(result) = context
                        .function_hook()
                        .and_then(|hook| hook.call_bound_builtin_function(binding, &arguments))
                    {
                        return result;
                    }
                    return eval_bound_builtin_function_call(
                        binding,
                        arguments,
                        &context.sql_context(),
                    );
                }
                let sql_context = context.sql_context();
                let engine = sql_context.engine.ok_or_else(|| {
                    SQLError::Unsupported(
                        "bound user function requires a logical engine session".into(),
                    )
                })?;
                engine
                    .call_bound_user_function(binding, &arguments)
                    .unwrap_or_else(|| Err(SQLError::UnknownFunction(binding.name.clone())))
            } else {
                eval_function_call(name, arguments, &context.sql_context())
            }
        }
        ScalarExpr::Array(items) => items
            .iter()
            .map(|item| eval_scalar(item, context))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|items| {
                ArrayValue::try_new(items).map(Value::Array).ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "multidimensional arrays must have matching dimensions".into(),
                    )
                })
            }),
        ScalarExpr::Row(items) => items
            .iter()
            .map(|item| eval_scalar(item, context))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Row),
        ScalarExpr::Binary { op, lhs, rhs } => {
            let left = eval_scalar(lhs, context)?;
            let right = eval_scalar(rhs, context)?;
            eval_binary_values_with_integer_width(
                *op,
                &left,
                &right,
                scalar_integer_binary_width(lhs, rhs),
            )
        }
        ScalarExpr::UnaryMinus(inner) => {
            let source_ty = scalar_source_type(inner, context);
            let value = eval_scalar(inner, context)?;
            negate_value(&value, source_ty.as_deref())
        }
        ScalarExpr::Not(inner) => {
            let value = eval_scalar(inner, context)?;
            if matches!(value, Value::Null) {
                Ok(Value::Null)
            } else {
                Ok(Value::Bool(!truthy(&value)))
            }
        }
        ScalarExpr::And(items) => eval_and(items, context),
        ScalarExpr::Or(items) => eval_or(items, context),
        ScalarExpr::IsNull { expr, negated } => {
            let is_null = matches!(eval_scalar(expr, context)?, Value::Null);
            Ok(Value::Bool(if *negated { !is_null } else { is_null }))
        }
        ScalarExpr::Between { expr, low, high } => eval_between(expr, low, high, context),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => eval_in_list(expr, list, *negated, context),
        ScalarExpr::WindowCall { name, .. } => Err(SQLError::Unsupported(format!(
            "window function `{name}` must be evaluated by the window-aware executor"
        ))),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => eval_case(base.as_deref(), when, else_branch.as_deref(), context),
        ScalarExpr::Cast { expr, ty } => {
            let source_ty = scalar_source_type(expr, context);
            let value = eval_scalar(expr, context)?;
            cast_value_with_type_resolution(
                &value,
                source_ty.as_deref(),
                ty,
                context.function_hook(),
            )
        }
        ScalarExpr::ScalarSubquery(subquery) => execute_scalar_subquery(*subquery, context),
        ScalarExpr::Exists { subquery, negated } => {
            let exists = execute_exists_subquery(*subquery, context)?;
            Ok(Value::Bool(if *negated { !exists } else { exists }))
        }
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => {
            let needle = eval_scalar(expr, context)?;
            let found = execute_in_subquery(*subquery, &needle, context)?;
            Ok(found.map_or(Value::Null, |found| {
                Value::Bool(if *negated { !found } else { found })
            }))
        }
    }
}

fn evaluate_qualified_whole_row(
    qualifier: &str,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    if let Some(schema) = context
        .row_schema()
        .filter(|schema| schema.has_qualifier(qualifier))
    {
        let row = context
            .row_lookup()
            .ok_or_else(|| SQLError::Internal("whole-row reference without row context".into()))?;
        return materialize_qualified_whole_row(schema, row, qualifier);
    }
    if let Some((schema, row)) = context
        .physical_outer_row()
        .filter(|(schema, _)| schema.has_qualifier(qualifier))
    {
        let view = schema.view(row);
        return materialize_qualified_whole_row(schema, &view, qualifier);
    }
    Err(SQLError::UnknownTable(qualifier.to_string()))
}

fn materialize_qualified_whole_row(
    schema: &crate::RowSchema,
    row: &dyn uqa_sql::expr::RowLookup,
    qualifier: &str,
) -> Result<Value, SQLError> {
    schema
        .qualified_star_position_layout(qualifier)
        .into_iter()
        .filter(|(column, logical, _, _)| {
            logical.map_or_else(
                || {
                    let mut matching = false;
                    let mut visible = false;
                    for (position, identity) in schema.identities().iter().enumerate() {
                        if identity.column() == column {
                            matching = true;
                            visible |= schema.wildcard_position_visible(position);
                        }
                    }
                    !matching || visible
                },
                |position| schema.wildcard_position_visible(position),
            )
        })
        .map(|(column, logical, _, _)| {
            let value = row
                .qualified_column(qualifier, &column)
                .or_else(|| logical.and_then(|position| row.positional_column(position)))
                .cloned()
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "whole-row attribute {qualifier}.{column} is unavailable"
                    ))
                })?;
            Ok((column, value))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Record)
}

fn eval_parameter(index: usize, params: &[SQLParam]) -> Result<Value, SQLError> {
    match index
        .checked_sub(1)
        .and_then(|parameter_index| params.get(parameter_index))
    {
        Some(SQLParam::Scalar(value) | SQLParam::TypedScalar { value, .. }) => Ok(value.clone()),
        Some(SQLParam::Vector(vector)) => Ok(Value::List(
            vector
                .iter()
                .map(|value| Value::Float(f64::from(*value)))
                .collect(),
        )),
        Some(SQLParam::Tensor(vectors)) => Ok(Value::List(
            vectors
                .iter()
                .map(|vector| {
                    Value::List(
                        vector
                            .iter()
                            .map(|value| Value::Float(f64::from(*value)))
                            .collect(),
                    )
                })
                .collect(),
        )),
        None => Err(SQLError::MissingParam(index)),
    }
}

fn eval_and(items: &[ScalarExpr], context: &ScalarEvalContext<'_>) -> Result<Value, SQLError> {
    let mut saw_null = false;
    for item in items {
        let value = eval_scalar(item, context)?;
        if matches!(value, Value::Null) {
            saw_null = true;
        } else if !truthy(&value) {
            return Ok(Value::Bool(false));
        }
    }
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(true)
    })
}

fn eval_or(items: &[ScalarExpr], context: &ScalarEvalContext<'_>) -> Result<Value, SQLError> {
    let mut saw_null = false;
    for item in items {
        let value = eval_scalar(item, context)?;
        if matches!(value, Value::Null) {
            saw_null = true;
        } else if truthy(&value) {
            return Ok(Value::Bool(true));
        }
    }
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(false)
    })
}

fn eval_between(
    expression: &ScalarExpr,
    low: &ScalarExpr,
    high: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    let value = eval_scalar(expression, context)?;
    let low = eval_scalar(low, context)?;
    let high = eval_scalar(high, context)?;
    let greater_equal = eval_binary_values(BinaryOp::GreaterEqual, &value, &low)?;
    let less_equal = eval_binary_values(BinaryOp::LessEqual, &value, &high)?;
    match (greater_equal, less_equal) {
        (Value::Bool(false), _) | (_, Value::Bool(false)) => Ok(Value::Bool(false)),
        (Value::Bool(true), Value::Bool(true)) => Ok(Value::Bool(true)),
        _ => Ok(Value::Null),
    }
}

fn eval_in_list(
    expression: &ScalarExpr,
    list: &[ScalarExpr],
    negated: bool,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    let needle = eval_scalar(expression, context)?;
    let mut saw_null = matches!(needle, Value::Null);
    for item in list {
        let candidate = eval_scalar(item, context)?;
        match eval_binary_values(BinaryOp::Equal, &needle, &candidate)? {
            Value::Bool(true) => return Ok(Value::Bool(!negated)),
            Value::Null => saw_null = true,
            _ => {}
        }
    }
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(negated)
    })
}

fn eval_case(
    base: Option<&ScalarExpr>,
    branches: &[(ScalarExpr, ScalarExpr)],
    else_branch: Option<&ScalarExpr>,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    let base = base
        .map(|expression| eval_scalar(expression, context))
        .transpose()?;
    for (condition, result) in branches {
        let condition = eval_scalar(condition, context)?;
        let matched = match &base {
            Some(base) => matches!(
                eval_binary_values(BinaryOp::Equal, base, &condition)?,
                Value::Bool(true)
            ),
            None => truthy(&condition),
        };
        if matched {
            return eval_scalar(result, context);
        }
    }
    else_branch.map_or(Ok(Value::Null), |expression| {
        eval_scalar(expression, context)
    })
}

fn execute_scalar_subquery(
    subquery: SubqueryId,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    let runner = context
        .subquery_runner()
        .ok_or_else(|| SQLError::Unsupported("physical subquery requires a plan runner".into()))?;
    match context.physical_outer_row() {
        Some((schema, row)) => {
            runner.scalar_subquery_value_physical(subquery, schema, row, context.params())
        }
        None => runner.scalar_subquery_value(subquery, context.outer_row(), context.params()),
    }
}

fn execute_exists_subquery(
    subquery: SubqueryId,
    context: &ScalarEvalContext<'_>,
) -> Result<bool, SQLError> {
    let runner = context
        .subquery_runner()
        .ok_or_else(|| SQLError::Unsupported("physical subquery requires a plan runner".into()))?;
    match context.physical_outer_row() {
        Some((schema, row)) => {
            runner.subquery_exists_physical(subquery, schema, row, context.params())
        }
        None => runner.subquery_exists(subquery, context.outer_row(), context.params()),
    }
}

fn execute_in_subquery(
    subquery: SubqueryId,
    needle: &Value,
    context: &ScalarEvalContext<'_>,
) -> Result<Option<bool>, SQLError> {
    let runner = context
        .subquery_runner()
        .ok_or_else(|| SQLError::Unsupported("physical subquery requires a plan runner".into()))?;
    match context.physical_outer_row() {
        Some((schema, row)) => {
            runner.subquery_contains_physical(subquery, needle, schema, row, context.params())
        }
        None => runner.subquery_contains(subquery, needle, context.outer_row(), context.params()),
    }
}

fn scalar_source_type(expression: &ScalarExpr, context: &ScalarEvalContext<'_>) -> Option<String> {
    match expression {
        ScalarExpr::Cast { ty, .. } => return Some(ty.clone()),
        ScalarExpr::UnaryMinus(inner) => return scalar_source_type(inner, context),
        ScalarExpr::Literal(Value::Int(value)) if i32::try_from(*value).is_ok() => {
            return Some("integer".into());
        }
        ScalarExpr::Literal(Value::Int(_)) => return Some("bigint".into()),
        ScalarExpr::Literal(Value::Bytes(_)) => return Some("bytea".into()),
        ScalarExpr::Literal(Value::Str(_) | Value::FixedChar(_)) => return None,
        _ => {}
    }
    context
        .row_schema()
        .and_then(|schema| {
            crate::scalar_type(expression, schema, context.params())
                .ok()
                .flatten()
        })
        .map(|ty| ty.sql_name())
}

fn scalar_integer_width(expression: &ScalarExpr) -> Option<IntegerWidth> {
    match expression {
        ScalarExpr::Literal(Value::Int(value)) => Some(integer_width_for_literal(*value)),
        ScalarExpr::Cast { ty, .. } => integer_width_for_type(ty),
        ScalarExpr::UnaryMinus(inner) => scalar_integer_width(inner),
        ScalarExpr::Binary {
            op: BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide,
            lhs,
            rhs,
        } => Some(scalar_integer_width(lhs)?.max(scalar_integer_width(rhs)?)),
        _ => None,
    }
}

pub(crate) fn scalar_integer_binary_width(
    lhs: &ScalarExpr,
    rhs: &ScalarExpr,
) -> Option<IntegerWidth> {
    Some(scalar_integer_width(lhs)?.max(scalar_integer_width(rhs)?))
}
