//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow-preserving evaluation of compiled projected predicates.

use uqa_core::Value;
use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::{
    cast_value_from, eval_binary_values, eval_binary_values_with_integer_width,
    eval_comparison_truth, negate_value, truthy,
};
use uqa_sql::SQLError;

use super::{ProjectedExpr, ProjectedIntPredicate};
use crate::PhysicalRowView;

static NULL_VALUE: Value = Value::Null;

trait FieldValues {
    fn field(&self, index: usize) -> &Value;
}

impl FieldValues for [&Value] {
    fn field(&self, index: usize) -> &Value {
        self.get(index).copied().unwrap_or(&NULL_VALUE)
    }
}

impl FieldValues for PhysicalRowView<'_> {
    fn field(&self, index: usize) -> &Value {
        self.value_at(index).unwrap_or(&NULL_VALUE)
    }
}

enum ProjectedValue<'a> {
    Borrowed(&'a Value),
    Owned(Value),
}

impl ProjectedValue<'_> {
    fn as_value(&self) -> &Value {
        match self {
            Self::Borrowed(value) => value,
            Self::Owned(value) => value,
        }
    }
}

pub(super) fn keep(expression: &ProjectedExpr, fields: &[&Value]) -> Result<bool, SQLError> {
    evaluate_truth(expression, fields).map(|truth| truth == Some(true))
}

pub(super) fn keep_row(
    expression: &ProjectedExpr,
    row: &PhysicalRowView<'_>,
) -> Result<bool, SQLError> {
    evaluate_truth(expression, row).map(|truth| truth == Some(true))
}

fn evaluate_truth<F: FieldValues + ?Sized>(
    expression: &ProjectedExpr,
    fields: &F,
) -> Result<Option<bool>, SQLError> {
    let truth = match expression {
        ProjectedExpr::Binary { op, lhs, rhs, .. } if is_comparison(*op) => {
            let lhs = evaluate(lhs, fields)?;
            let rhs = evaluate(rhs, fields)?;
            eval_comparison_truth(*op, lhs.as_value(), rhs.as_value())?
        }
        ProjectedExpr::IntFieldComparison {
            field,
            op,
            literal,
            field_on_left,
        } => evaluate_int_comparison_truth(fields.field(*field), *op, *literal, *field_on_left)?,
        ProjectedExpr::Not(expression) => evaluate_truth(expression, fields)?.map(|value| !value),
        ProjectedExpr::And(items) => evaluate_truth_and(items, fields)?,
        ProjectedExpr::IntFieldConjunction(items) => evaluate_int_conjunction_truth(items, fields)?,
        ProjectedExpr::Or(items) => evaluate_truth_or(items, fields)?,
        ProjectedExpr::IsNull {
            expression,
            negated,
        } => Some(matches!(evaluate(expression, fields)?.as_value(), Value::Null) != *negated),
        ProjectedExpr::Between {
            expression,
            low,
            high,
        } => evaluate_between_truth(expression, low, high, fields)?,
        ProjectedExpr::IntFieldBetween { field, low, high } => {
            evaluate_int_between_truth(fields.field(*field), *low, *high)?
        }
        ProjectedExpr::InList {
            expression,
            list,
            negated,
        } => evaluate_in_list_truth(expression, list, *negated, fields)?,
        ProjectedExpr::Like {
            expression,
            pattern,
        } => {
            let value = evaluate(expression, fields)?;
            if matches!(value.as_value(), Value::Null) {
                None
            } else {
                Some(pattern.matches_value(value.as_value()))
            }
        }
        _ => {
            let value = evaluate(expression, fields)?;
            value_to_truth(value.as_value())
        }
    };
    Ok(truth)
}

#[inline]
fn is_comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    )
}

#[inline]
fn value_to_truth(value: &Value) -> Option<bool> {
    if matches!(value, Value::Null) {
        None
    } else {
        Some(truthy(value))
    }
}

fn evaluate<'a, F: FieldValues + ?Sized>(
    expression: &'a ProjectedExpr,
    fields: &'a F,
) -> Result<ProjectedValue<'a>, SQLError> {
    let value = match expression {
        ProjectedExpr::Field(index) => ProjectedValue::Borrowed(fields.field(*index)),
        ProjectedExpr::Literal(value) => ProjectedValue::Borrowed(value),
        ProjectedExpr::Binary {
            op,
            lhs,
            rhs,
            integer_width,
        } => {
            let lhs = evaluate(lhs, fields)?;
            let rhs = evaluate(rhs, fields)?;
            ProjectedValue::Owned(eval_binary_values_with_integer_width(
                *op,
                lhs.as_value(),
                rhs.as_value(),
                *integer_width,
            )?)
        }
        ProjectedExpr::UnaryMinus(expression) => {
            let source_ty = projected_source_type(expression);
            let value = evaluate(expression, fields)?;
            ProjectedValue::Owned(negate_value(value.as_value(), source_ty)?)
        }
        ProjectedExpr::IntFieldComparison {
            field,
            op,
            literal,
            field_on_left,
        } => ProjectedValue::Owned(evaluate_int_comparison(
            fields.field(*field),
            *op,
            *literal,
            *field_on_left,
        )?),
        ProjectedExpr::Not(expression) => {
            let value = evaluate(expression, fields)?;
            ProjectedValue::Owned(match value.as_value() {
                Value::Null => Value::Null,
                value => Value::Bool(!truthy(value)),
            })
        }
        ProjectedExpr::And(items) => ProjectedValue::Owned(evaluate_and(items, fields)?),
        ProjectedExpr::IntFieldConjunction(items) => ProjectedValue::Owned(truth_to_value(
            evaluate_int_conjunction_truth(items, fields)?,
        )),
        ProjectedExpr::Or(items) => ProjectedValue::Owned(evaluate_or(items, fields)?),
        ProjectedExpr::IsNull {
            expression,
            negated,
        } => ProjectedValue::Owned(Value::Bool(
            matches!(evaluate(expression, fields)?.as_value(), Value::Null) != *negated,
        )),
        ProjectedExpr::Between {
            expression,
            low,
            high,
        } => ProjectedValue::Owned(evaluate_between(expression, low, high, fields)?),
        ProjectedExpr::IntFieldBetween { field, low, high } => {
            ProjectedValue::Owned(evaluate_int_between(fields.field(*field), *low, *high)?)
        }
        ProjectedExpr::InList {
            expression,
            list,
            negated,
        } => ProjectedValue::Owned(evaluate_in_list(expression, list, *negated, fields)?),
        ProjectedExpr::Like {
            expression,
            pattern,
        } => {
            let value = evaluate(expression, fields)?;
            ProjectedValue::Owned(if matches!(value.as_value(), Value::Null) {
                Value::Null
            } else {
                Value::Bool(pattern.matches_value(value.as_value()))
            })
        }
        ProjectedExpr::Cast { expression, ty } => {
            let source_ty = match expression.as_ref() {
                ProjectedExpr::Cast { ty, .. } => Some(ty.as_str()),
                ProjectedExpr::Literal(Value::Int(_)) => Some("integer"),
                ProjectedExpr::Literal(Value::Bytes(_)) => Some("bytea"),
                _ => None,
            };
            let value = evaluate(expression, fields)?;
            ProjectedValue::Owned(cast_value_from(value.as_value(), ty, source_ty)?)
        }
    };
    Ok(value)
}

fn projected_source_type(expression: &ProjectedExpr) -> Option<&str> {
    match expression {
        ProjectedExpr::Cast { ty, .. } => Some(ty),
        ProjectedExpr::UnaryMinus(inner) => projected_source_type(inner),
        ProjectedExpr::Literal(Value::Int(value)) if i32::try_from(*value).is_ok() => {
            Some("integer")
        }
        ProjectedExpr::Literal(Value::Int(_)) => Some("bigint"),
        ProjectedExpr::Literal(Value::Bytes(_)) => Some("bytea"),
        _ => None,
    }
}

fn evaluate_int_comparison(
    field: &Value,
    op: BinaryOp,
    literal: i64,
    field_on_left: bool,
) -> Result<Value, SQLError> {
    Ok(truth_to_value(evaluate_int_comparison_truth(
        field,
        op,
        literal,
        field_on_left,
    )?))
}

fn evaluate_int_comparison_truth(
    field: &Value,
    op: BinaryOp,
    literal: i64,
    field_on_left: bool,
) -> Result<Option<bool>, SQLError> {
    let Value::Int(field) = field else {
        if matches!(field, Value::Null) {
            return Ok(None);
        }
        let literal = Value::Int(literal);
        return if field_on_left {
            eval_comparison_truth(op, field, &literal)
        } else {
            eval_comparison_truth(op, &literal, field)
        };
    };
    let (lhs, rhs) = if field_on_left {
        (*field, literal)
    } else {
        (literal, *field)
    };
    let result = match op {
        BinaryOp::Equal => lhs == rhs,
        BinaryOp::NotEqual => lhs != rhs,
        BinaryOp::Less => lhs < rhs,
        BinaryOp::LessEqual => lhs <= rhs,
        BinaryOp::Greater => lhs > rhs,
        BinaryOp::GreaterEqual => lhs >= rhs,
        _ => {
            return Err(SQLError::Internal(format!(
                "non-comparison operator {op:?} reached integer predicate"
            )))
        }
    };
    Ok(Some(result))
}

fn evaluate_int_between(field: &Value, low: i64, high: i64) -> Result<Value, SQLError> {
    Ok(truth_to_value(evaluate_int_between_truth(
        field, low, high,
    )?))
}

fn evaluate_int_between_truth(
    field: &Value,
    low: i64,
    high: i64,
) -> Result<Option<bool>, SQLError> {
    match field {
        Value::Null => Ok(None),
        Value::Int(value) => Ok(Some(*value >= low && *value <= high)),
        value => {
            let low = Value::Int(low);
            let high = Value::Int(high);
            let lower = eval_comparison_truth(BinaryOp::GreaterEqual, value, &low)?;
            let upper = eval_comparison_truth(BinaryOp::LessEqual, value, &high)?;
            Ok(and_truth(lower, upper))
        }
    }
}

fn evaluate_truth_and<F: FieldValues + ?Sized>(
    items: &[ProjectedExpr],
    fields: &F,
) -> Result<Option<bool>, SQLError> {
    let mut saw_null = false;
    for item in items {
        match evaluate_truth(item, fields)? {
            Some(false) => return Ok(Some(false)),
            None => saw_null = true,
            Some(true) => {}
        }
    }
    Ok(if saw_null { None } else { Some(true) })
}

fn evaluate_int_conjunction_truth<F: FieldValues + ?Sized>(
    items: &[ProjectedIntPredicate],
    fields: &F,
) -> Result<Option<bool>, SQLError> {
    let mut saw_null = false;
    for item in items {
        let truth = match item {
            ProjectedIntPredicate::Comparison {
                field,
                op,
                literal,
                field_on_left,
            } => {
                evaluate_int_comparison_truth(fields.field(*field), *op, *literal, *field_on_left)?
            }
            ProjectedIntPredicate::Between { field, low, high } => {
                evaluate_int_between_truth(fields.field(*field), *low, *high)?
            }
        };
        match truth {
            Some(false) => return Ok(Some(false)),
            None => saw_null = true,
            Some(true) => {}
        }
    }
    Ok(if saw_null { None } else { Some(true) })
}

fn evaluate_truth_or<F: FieldValues + ?Sized>(
    items: &[ProjectedExpr],
    fields: &F,
) -> Result<Option<bool>, SQLError> {
    let mut saw_null = false;
    for item in items {
        match evaluate_truth(item, fields)? {
            Some(true) => return Ok(Some(true)),
            None => saw_null = true,
            Some(false) => {}
        }
    }
    Ok(if saw_null { None } else { Some(false) })
}

fn evaluate_and<F: FieldValues + ?Sized>(
    items: &[ProjectedExpr],
    fields: &F,
) -> Result<Value, SQLError> {
    let mut saw_null = false;
    for item in items {
        let value = evaluate(item, fields)?;
        match value.as_value() {
            Value::Null => saw_null = true,
            value if !truthy(value) => return Ok(Value::Bool(false)),
            _ => {}
        }
    }
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(true)
    })
}

fn evaluate_or<F: FieldValues + ?Sized>(
    items: &[ProjectedExpr],
    fields: &F,
) -> Result<Value, SQLError> {
    let mut saw_null = false;
    for item in items {
        let value = evaluate(item, fields)?;
        match value.as_value() {
            Value::Null => saw_null = true,
            value if truthy(value) => return Ok(Value::Bool(true)),
            _ => {}
        }
    }
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(false)
    })
}

fn evaluate_between<F: FieldValues + ?Sized>(
    expression: &ProjectedExpr,
    low: &ProjectedExpr,
    high: &ProjectedExpr,
    fields: &F,
) -> Result<Value, SQLError> {
    let value = evaluate(expression, fields)?;
    let low = evaluate(low, fields)?;
    let high = evaluate(high, fields)?;
    let lower = eval_binary_values(BinaryOp::GreaterEqual, value.as_value(), low.as_value())?;
    let upper = eval_binary_values(BinaryOp::LessEqual, value.as_value(), high.as_value())?;
    Ok(match (lower, upper) {
        (Value::Bool(false), _) | (_, Value::Bool(false)) => Value::Bool(false),
        (Value::Bool(true), Value::Bool(true)) => Value::Bool(true),
        _ => Value::Null,
    })
}

fn evaluate_between_truth<F: FieldValues + ?Sized>(
    expression: &ProjectedExpr,
    low: &ProjectedExpr,
    high: &ProjectedExpr,
    fields: &F,
) -> Result<Option<bool>, SQLError> {
    let value = evaluate(expression, fields)?;
    let low = evaluate(low, fields)?;
    let high = evaluate(high, fields)?;
    let lower = eval_comparison_truth(BinaryOp::GreaterEqual, value.as_value(), low.as_value())?;
    let upper = eval_comparison_truth(BinaryOp::LessEqual, value.as_value(), high.as_value())?;
    Ok(and_truth(lower, upper))
}

fn evaluate_in_list<F: FieldValues + ?Sized>(
    expression: &ProjectedExpr,
    list: &[ProjectedExpr],
    negated: bool,
    fields: &F,
) -> Result<Value, SQLError> {
    let needle = evaluate(expression, fields)?;
    let mut saw_null = matches!(needle.as_value(), Value::Null);
    for candidate in list {
        let candidate = evaluate(candidate, fields)?;
        match eval_binary_values(BinaryOp::Equal, needle.as_value(), candidate.as_value())? {
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

fn evaluate_in_list_truth<F: FieldValues + ?Sized>(
    expression: &ProjectedExpr,
    list: &[ProjectedExpr],
    negated: bool,
    fields: &F,
) -> Result<Option<bool>, SQLError> {
    let needle = evaluate(expression, fields)?;
    let mut saw_null = matches!(needle.as_value(), Value::Null);
    for candidate in list {
        let candidate = evaluate(candidate, fields)?;
        match eval_comparison_truth(BinaryOp::Equal, needle.as_value(), candidate.as_value())? {
            Some(true) => return Ok(Some(!negated)),
            None => saw_null = true,
            Some(false) => {}
        }
    }
    Ok(if saw_null { None } else { Some(negated) })
}

#[inline]
fn and_truth(lhs: Option<bool>, rhs: Option<bool>) -> Option<bool> {
    match (lhs, rhs) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

#[inline]
fn truth_to_value(truth: Option<bool>) -> Value {
    truth.map(Value::Bool).unwrap_or(Value::Null)
}
