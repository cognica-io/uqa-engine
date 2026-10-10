//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct evaluation operations for the physical scalar IR.

use crate::RowSchemaExecution;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::{truthy, IntegerWidth};
use uqa_sql::{SQLError, SQLParam};

use super::call_arguments::eval_call_arguments_with_control;
use super::context::ScalarEvalContext;
use super::{ScalarExpr, SubqueryId};

/// Evaluate the physical scalar tree directly. No parser expression is reconstructed at this boundary.
pub fn eval_scalar(
    expression: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    eval_scalar_inner(expression, context, &ProductionControl::uncontrolled())
        .map(|value| value.into_uncontrolled().expect("ordinary scalar result"))
}

/// Evaluate a validated generated-column expression against borrowed row fields. The context has no query runner, session callbacks or physical whole-row schema; their general execution contracts remain with `eval_scalar`.
pub fn eval_generated_scalar_with_control(
    expression: &ScalarExpr,
    row: &dyn uqa_sql::expr::RowLookup,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    eval_scalar_inner(
        expression,
        &ScalarEvalContext::from_row_lookup(row, &[]),
        control,
    )
}

pub(super) fn eval_scalar_inner(
    expression: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    match expression {
        ScalarExpr::Default => Err(SQLError::Internal(
            "DEFAULT reached scalar expression evaluation without a mutation target".into(),
        )),
        ScalarExpr::Star => Err(SQLError::Internal("`*` cannot be evaluated".into())),
        ScalarExpr::QualifiedStar(qualifier) => {
            ordinary_output(evaluate_qualified_whole_row(qualifier, context), control)
        }
        ScalarExpr::Column(name) => column(name, context, control),
        ScalarExpr::Position(position) => context
            .row_lookup()
            .and_then(|row| row.positional_column(*position))
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "bound physical column position {position} is unavailable"
                ))
            })
            .and_then(|value| control.copy_value(value).map_err(Into::into)),
        ScalarExpr::InternalColumn(column) => context
            .row_lookup()
            .and_then(|row| row.internal_column(*column))
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "internal relation attribute {column:?} is unavailable"
                ))
            })
            .and_then(|value| control.copy_value(value).map_err(Into::into)),
        ScalarExpr::QualifiedColumn { qualifier, column } => context
            .sql_context()
            .qualified_column_value_with_control(qualifier, column, control),
        ScalarExpr::TypedLiteral { value, ty, .. } => {
            uqa_sql::expr::composites::literal::evaluate_with_control(
                value,
                ty,
                context.function_hook(),
                control,
            )
        }
        ScalarExpr::Literal(value) => control.copy_value(value).map_err(Into::into),
        ScalarExpr::Param(index) => eval_parameter(*index, context.params(), control),
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => evaluate_function(name, binding.as_ref(), args, context, control),
        ScalarExpr::Array(items) => array(items, context, control),
        ScalarExpr::CompositeRow { items, binding, .. } => {
            uqa_sql::expr::composites::constructor::evaluate_with_control(
                binding,
                items.len(),
                context.function_hook(),
                control,
                |index| eval_scalar_inner(&items[index], context, control),
            )
        }
        ScalarExpr::Row(items) => row(items, context, control),
        ScalarExpr::Binary { op, lhs, rhs } => binary(*op, lhs, rhs, context, control),
        ScalarExpr::UnaryMinus(inner) => unary_minus(inner, context, control),
        ScalarExpr::Not(inner) => not(inner, context, control),
        ScalarExpr::And(items) => eval_and(items, context, control),
        ScalarExpr::Or(items) => eval_or(items, context, control),
        ScalarExpr::IsNull { expr, negated } => is_null(expr, *negated, context, control),
        ScalarExpr::Between { expr, low, high } => eval_between(expr, low, high, context, control),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => eval_in_list(expr, list, *negated, context, control),
        ScalarExpr::WindowCall { name, .. } => Err(SQLError::Unsupported(format!(
            "window function `{name}` must be evaluated by the window-aware executor"
        ))),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => eval_case(
            base.as_deref(),
            when,
            else_branch.as_deref(),
            context,
            control,
        ),
        ScalarExpr::Cast { expr, ty, .. } => cast(expr, ty, context, control),
        ScalarExpr::ScalarSubquery(subquery) => {
            ordinary_output(execute_scalar_subquery(*subquery, context), control)
        }
        ScalarExpr::Exists { subquery, negated } => exists(*subquery, *negated, context, control),
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => in_subquery(expr, *subquery, *negated, context, control),
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
                    // A whole-row value holds user attributes only; scans expose system columns through qualified aliases.
                    if uqa_sql::schema::columns::POSTGRES_SYSTEM_COLUMNS.contains(&column.as_str())
                    {
                        return false;
                    }
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
        .map(|fields| Value::Record(fields.into()))
}

fn eval_parameter(
    index: usize,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    params
        .get(index.checked_sub(1).ok_or(SQLError::MissingParam(index))?)
        .ok_or(SQLError::MissingParam(index))?
        .to_value_with_control(control)
}

fn eval_and(
    items: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let mut saw_null = false;
    for item in items {
        let value = eval_scalar_inner(item, context, control)?;
        if matches!(*value, Value::Null) {
            saw_null = true;
        } else if !truthy(&value) {
            return plain(Value::Bool(false), control);
        }
    }
    plain(
        if saw_null {
            Value::Null
        } else {
            Value::Bool(true)
        },
        control,
    )
}

fn eval_or(
    items: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let mut saw_null = false;
    for item in items {
        let value = eval_scalar_inner(item, context, control)?;
        if matches!(*value, Value::Null) {
            saw_null = true;
        } else if truthy(&value) {
            return plain(Value::Bool(true), control);
        }
    }
    plain(
        if saw_null {
            Value::Null
        } else {
            Value::Bool(false)
        },
        control,
    )
}

fn eval_between(
    expression: &ScalarExpr,
    low: &ScalarExpr,
    high: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    eval_between_states(expression, low, high, context, control, |index| {
        context.enum_binary_comparison_state(if index == 0 { low } else { high })
    })
}

fn eval_between_states<'a>(
    expression: &ScalarExpr,
    low: &ScalarExpr,
    high: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
    state: impl Fn(usize) -> Option<&'a uqa_sql::expr::enums::EnumComparisonState>,
) -> Result<Produced<Value>, SQLError> {
    let ge = {
        let value = eval_scalar_inner(expression, context, control)?;
        let low = eval_scalar_inner(low, context, control)?;
        compare_values(
            BinaryOp::GreaterEqual,
            &value,
            &low,
            context,
            control,
            || state(0),
        )?
    };
    if ge == Some(false) {
        return plain(Value::Bool(false), control);
    }
    let value = eval_scalar_inner(expression, context, control)?;
    let high = eval_scalar_inner(high, context, control)?;
    let le = compare_values(BinaryOp::LessEqual, &value, &high, context, control, || {
        state(1)
    })?;
    plain(
        match (ge, le) {
            (_, Some(false)) => Value::Bool(false),
            (Some(true), Some(true)) => Value::Bool(true),
            _ => Value::Null,
        },
        control,
    )
}

fn eval_between_symmetric(
    args: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let [value, low, high] = args else {
        return Err(SQLError::TypeMismatch(
            "BETWEEN SYMMETRIC takes 3 args".into(),
        ));
    };
    let states = context.enum_comparison_states(args);
    let forward = eval_between_states(value, low, high, context, control, |index| states[index])?;
    if *forward == Value::Bool(true) {
        return Ok(forward);
    }
    let backward = eval_between_states(value, high, low, context, control, |index| {
        states[index + 2]
    })?;
    plain(
        match (&*forward, &*backward) {
            (_, Value::Bool(true)) => Value::Bool(true),
            (Value::Null, _) | (_, Value::Null) => Value::Null,
            _ => Value::Bool(false),
        },
        control,
    )
}

fn compare_values<'a>(
    op: BinaryOp,
    left: &Value,
    right: &Value,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
    state: impl FnOnce() -> Option<&'a uqa_sql::expr::enums::EnumComparisonState>,
) -> Result<Option<bool>, SQLError> {
    uqa_sql::expr::eval_comparison_truth_with_deferred_state(
        op,
        left,
        right,
        control,
        context.function_hook(),
        state,
    )
}

fn eval_in_list(
    expression: &ScalarExpr,
    list: &[ScalarExpr],
    negated: bool,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let needle = eval_scalar_inner(expression, context, control)?;
    let mut saw_null = matches!(*needle, Value::Null);
    for item in list {
        let candidate = eval_scalar_inner(item, context, control)?;
        match compare_values(
            BinaryOp::Equal,
            &needle,
            &candidate,
            context,
            control,
            || None,
        )? {
            Some(true) => return plain(Value::Bool(!negated), control),
            None => saw_null = true,
            _ => {}
        }
    }
    plain(
        if saw_null {
            Value::Null
        } else {
            Value::Bool(negated)
        },
        control,
    )
}

fn eval_case(
    base: Option<&ScalarExpr>,
    branches: &[(ScalarExpr, ScalarExpr)],
    else_branch: Option<&ScalarExpr>,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let base = base
        .map(|expression| eval_scalar_inner(expression, context, control))
        .transpose()?;
    for (condition, result) in branches {
        let condition = eval_scalar_inner(condition, context, control)?;
        let matched = match &base {
            Some(base) => {
                compare_values(BinaryOp::Equal, base, &condition, context, control, || None)?
                    == Some(true)
            }
            None => truthy(&condition),
        };
        if matched {
            return eval_scalar_inner(result, context, control);
        }
    }
    else_branch.map_or(plain(Value::Null, control), |expression| {
        eval_scalar_inner(expression, context, control)
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

fn scalar_source_type(
    expression: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<String>>, SQLError> {
    context.with_type_schema(|schema| {
        uqa_sql::scalar_operand_type_name_with_control(
            expression,
            schema,
            context.params(),
            control,
        )
    })
}

fn real_type_name(name: &str, control: &ProductionControl<'_>) -> Result<bool, SQLError> {
    match uqa_sql::ColumnType::from_sql_name_with_control(name, control) {
        Ok(ty) => Ok(matches!(*ty, uqa_sql::ColumnType::Real)),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(false),
    }
}

pub(crate) fn scalar_integer_binary_width(
    lhs: &ScalarExpr,
    rhs: &ScalarExpr,
    schema: &crate::RowSchema,
    parameters: &[SQLParam],
) -> Option<IntegerWidth> {
    uqa_sql::scalar_integer_operation_width(lhs, rhs, schema, parameters)
}

fn plain(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>, SQLError> {
    control
        .finish(value, control.empty_reservation())
        .map_err(Into::into)
}

// Whole physical rows, session functions and query runners exist only in the ordinary context. The generated entry constructs a field-only context, so these legacy capability calls fail before returning a value there.
fn ordinary_output(
    value: Result<Value, SQLError>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.finish(value?, None).map_err(Into::into)
}

fn evaluate_items(
    items: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    output.reserve(items.len())?;
    for item in items {
        output.push_produced(eval_scalar_inner(item, context, control)?)?;
    }
    output.finish().map_err(Into::into)
}

mod function;
mod variants;
use function::evaluate_function;
use variants::{array, binary, cast, column, exists, in_subquery, is_null, not, row, unary_minus};

#[cfg(test)]
mod comparison_tests;
#[cfg(test)]
mod production_tests;
#[cfg(test)]
mod stack_tests;
