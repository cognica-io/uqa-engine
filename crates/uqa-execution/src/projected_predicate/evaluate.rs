//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow-preserving evaluation of compiled projected predicates.

use uqa_core::Value;
use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::{cast_value, eval_binary_values, truthy};
use uqa_sql::SQLError;

use super::ProjectedExpr;

static NULL_VALUE: Value = Value::Null;

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
    evaluate(expression, fields).map(|value| truthy(value.as_value()))
}

fn evaluate<'a>(
    expression: &'a ProjectedExpr,
    fields: &'a [&'a Value],
) -> Result<ProjectedValue<'a>, SQLError> {
    let value = match expression {
        ProjectedExpr::Field(index) => {
            ProjectedValue::Borrowed(fields.get(*index).copied().unwrap_or(&NULL_VALUE))
        }
        ProjectedExpr::Literal(value) => ProjectedValue::Borrowed(value),
        ProjectedExpr::Binary { op, lhs, rhs } => {
            let lhs = evaluate(lhs, fields)?;
            let rhs = evaluate(rhs, fields)?;
            ProjectedValue::Owned(eval_binary_values(*op, lhs.as_value(), rhs.as_value())?)
        }
        ProjectedExpr::IntFieldComparison {
            field,
            op,
            literal,
            field_on_left,
        } => ProjectedValue::Owned(evaluate_int_comparison(
            fields.get(*field).copied().unwrap_or(&NULL_VALUE),
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
            ProjectedValue::Owned(evaluate_int_between(
                fields.get(*field).copied().unwrap_or(&NULL_VALUE),
                *low,
                *high,
            )?)
        }
        ProjectedExpr::InList {
            expression,
            list,
            negated,
        } => ProjectedValue::Owned(evaluate_in_list(expression, list, *negated, fields)?),
        ProjectedExpr::Cast { expression, ty } => {
            let value = evaluate(expression, fields)?;
            ProjectedValue::Owned(cast_value(value.as_value(), ty)?)
        }
    };
    Ok(value)
}

fn evaluate_int_comparison(
    field: &Value,
    op: BinaryOp,
    literal: i64,
    field_on_left: bool,
) -> Result<Value, SQLError> {
    let Value::Int(field) = field else {
        if matches!(field, Value::Null) {
            return Ok(Value::Null);
        }
        let literal = Value::Int(literal);
        return if field_on_left {
            eval_binary_values(op, field, &literal)
        } else {
            eval_binary_values(op, &literal, field)
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
    Ok(Value::Bool(result))
}

fn evaluate_int_between(field: &Value, low: i64, high: i64) -> Result<Value, SQLError> {
    match field {
        Value::Null => Ok(Value::Null),
        Value::Int(value) => Ok(Value::Bool(*value >= low && *value <= high)),
        value => {
            let low = Value::Int(low);
            let high = Value::Int(high);
            let lower = eval_binary_values(BinaryOp::GreaterEqual, value, &low)?;
            let upper = eval_binary_values(BinaryOp::LessEqual, value, &high)?;
            Ok(match (lower, upper) {
                (Value::Bool(false), _) | (_, Value::Bool(false)) => Value::Bool(false),
                (Value::Bool(true), Value::Bool(true)) => Value::Bool(true),
                _ => Value::Null,
            })
        }
    }
}

fn evaluate_and(items: &[ProjectedExpr], fields: &[&Value]) -> Result<Value, SQLError> {
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

fn evaluate_or(items: &[ProjectedExpr], fields: &[&Value]) -> Result<Value, SQLError> {
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

fn evaluate_between(
    expression: &ProjectedExpr,
    low: &ProjectedExpr,
    high: &ProjectedExpr,
    fields: &[&Value],
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

fn evaluate_in_list(
    expression: &ProjectedExpr,
    list: &[ProjectedExpr],
    negated: bool,
    fields: &[&Value],
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
