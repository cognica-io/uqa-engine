//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound comparison syntax uses shared SQL comparison and native array traversal owners.

use super::super::{eval_comparison_truth_with_engine, value_to_string_with_control, EvalContext};
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
    evaluate_with_context(dispatch, args, control, &EvalContext::new(None, &[]))
}

pub(in crate::expr) fn evaluate_with_context(
    dispatch: FunctionDispatch,
    args: &[Value],
    control: &ProductionControl<'_>,
    context: &EvalContext<'_>,
) -> Option<Result<Produced<Value>>> {
    let result = match dispatch {
        FunctionDispatch::AnyOperator => any_all(args, true, control, context),
        FunctionDispatch::AllOperator => any_all(args, false, control, context),
        FunctionDispatch::IsDistinct => is_distinct(args, control, context),
        FunctionDispatch::BetweenSymmetric => between_symmetric(args, control, context),
        _ => return None,
    };
    Some(result.and_then(|value| Ok(control.finish(value, control.empty_reservation())?)))
}

fn any_all(
    args: &[Value],
    is_any: bool,
    control: &ProductionControl<'_>,
    context: &EvalContext<'_>,
) -> Result<Value> {
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
    let Some(array) = args[1].array_view() else {
        if matches!(args[1], Value::Null) {
            return Ok(Value::Null);
        }
        return Err(SQLError::TypeMismatch("ANY/ALL requires an array".into()));
    };
    let mut saw_null = false;
    let mut elements = array.elements_with_control(control)?;
    while let Some(item) = elements.next_element()? {
        match compare(op, &args[0], item, control, context, 0)? {
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

fn is_distinct(
    args: &[Value],
    control: &ProductionControl<'_>,
    context: &EvalContext<'_>,
) -> Result<Value> {
    control.check()?;
    let [left, right] = args else {
        return Err(SQLError::TypeMismatch(
            "IS DISTINCT FROM takes 2 args".into(),
        ));
    };
    let distinct = match (left, right) {
        (Value::Null, Value::Null) => false,
        (Value::Null, _) | (_, Value::Null) => true,
        (left, right) => compare(BinaryOp::Equal, left, right, control, context, 0)? != Some(true),
    };
    Ok(Value::Bool(distinct))
}

fn between_symmetric(
    args: &[Value],
    control: &ProductionControl<'_>,
    context: &EvalContext<'_>,
) -> Result<Value> {
    control.check()?;
    let [value, low, high] = args else {
        return Err(SQLError::TypeMismatch(
            "BETWEEN SYMMETRIC takes 3 args".into(),
        ));
    };
    let forward = between(value, low, high, control, context, 0)?;
    if forward == Value::Bool(true) {
        return Ok(forward);
    }
    let backward = between(value, high, low, control, context, 2)?;
    Ok(match (forward, backward) {
        (Value::Bool(true), _) | (_, Value::Bool(true)) => Value::Bool(true),
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        _ => Value::Bool(false),
    })
}

fn between(
    value: &Value,
    low: &Value,
    high: &Value,
    control: &ProductionControl<'_>,
    context: &EvalContext<'_>,
    slot: usize,
) -> Result<Value> {
    let ge = compare(BinaryOp::GreaterEqual, value, low, control, context, slot)?;
    if ge == Some(false) {
        return Ok(Value::Bool(false));
    }
    let le = compare(BinaryOp::LessEqual, value, high, control, context, slot + 1)?;
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
    control: &ProductionControl<'_>,
    context: &EvalContext<'_>,
    slot: usize,
) -> Result<Option<bool>> {
    eval_comparison_truth_with_engine(
        op,
        left,
        right,
        control,
        context.engine,
        context.enum_comparison_state_at(slot),
    )
}

#[cfg(test)]
mod tests;
