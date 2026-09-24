//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound comparison syntax uses shared SQL comparison and native array traversal owners.

use super::super::{
    eval_between_with_control, eval_comparison_truth_with_control, value_to_string_with_control,
    values_equal_with_control,
};
use crate::{
    ast::{BinaryOp, FunctionDispatch},
    error::{Result, SQLError},
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

pub(super) fn evaluate(
    dispatch: FunctionDispatch,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    let result = match dispatch {
        FunctionDispatch::AnyOperator => any_all(args, true, control),
        FunctionDispatch::AllOperator => any_all(args, false, control),
        FunctionDispatch::IsDistinct => is_distinct(args, control),
        FunctionDispatch::BetweenSymmetric => between_symmetric(args, control),
        _ => return None,
    };
    Some(result.and_then(|value| Ok(control.finish(value, control.empty_reservation())?)))
}

fn any_all(args: &[Value], is_any: bool, control: &ProductionControl<'_>) -> Result<Value> {
    control.check()?;
    if args.len() != 3 {
        return Err(SQLError::TypeMismatch("ANY/ALL takes 3 args".into()));
    }
    let op = match value_to_string_with_control(&args[2], control)?.as_str() {
        "=" => BinaryOp::Equal,
        "<>" | "!=" => BinaryOp::NotEqual,
        "<" => BinaryOp::Less,
        "<=" => BinaryOp::LessEqual,
        ">" => BinaryOp::Greater,
        ">=" => BinaryOp::GreaterEqual,
        other => {
            return Err(SQLError::Unsupported(format!(
                "operator `{other}` with ANY/ALL"
            )))
        }
    };
    let Value::Array(array) = &args[1] else {
        if matches!(args[1], Value::Null) {
            return Ok(Value::Null);
        }
        return Err(SQLError::TypeMismatch("ANY/ALL requires an array".into()));
    };
    let mut saw_null = false;
    let mut elements = array.elements_with_control(control)?;
    while let Some(item) = elements.next_element()? {
        match eval_comparison_truth_with_control(op, &args[0], item, control)? {
            Some(true) if is_any => return Ok(Value::Bool(true)),
            Some(false) if !is_any => return Ok(Value::Bool(false)),
            None => saw_null = true,
            _ => {}
        }
    }
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(!is_any)
    })
}

fn is_distinct(args: &[Value], control: &ProductionControl<'_>) -> Result<Value> {
    control.check()?;
    let [left, right] = args else {
        return Err(SQLError::TypeMismatch(
            "IS DISTINCT FROM takes 2 args".into(),
        ));
    };
    let distinct = match (left, right) {
        (Value::Null, Value::Null) => false,
        (Value::Null, _) | (_, Value::Null) => true,
        (left, right) => !values_equal_with_control(left, right, control)?,
    };
    Ok(Value::Bool(distinct))
}

fn between_symmetric(args: &[Value], control: &ProductionControl<'_>) -> Result<Value> {
    control.check()?;
    let [value, low, high] = args else {
        return Err(SQLError::TypeMismatch(
            "BETWEEN SYMMETRIC takes 3 args".into(),
        ));
    };
    let forward = eval_between_with_control(value, low, high, control)?;
    let backward = eval_between_with_control(value, high, low, control)?;
    Ok(match (forward, backward) {
        (Value::Bool(true), _) | (_, Value::Bool(true)) => Value::Bool(true),
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        _ => Value::Bool(false),
    })
}

#[cfg(test)]
mod tests;
