//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core `PostgreSQL` array built-ins.

use super::{array_dimensions, out_of_range, to_i64, Result, SQLError, Value};

pub(super) fn eval_array_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    const NAMES: &[&str] = &[
        "array_length",
        "array_upper",
        "array_lower",
        "cardinality",
        "array_cat",
        "array_append",
        "array_prepend",
        "array_remove",
        "array_position",
        "unnest",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Value> {
        match name {
            // -------------------------------------------------------------
            // Array functions
            // -------------------------------------------------------------
            "array_length" | "array_upper" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch(format!("{name} takes 2 args")));
                }
                match &args[0] {
                    Value::List(items) => {
                        if matches!(args[1], Value::Null) || items.is_empty() {
                            return Ok(Value::Null);
                        }
                        let dimension = to_i64(&args[1])?;
                        if dimension <= 0 {
                            return Ok(Value::Null);
                        }
                        let Ok(index) = usize::try_from(dimension - 1) else {
                            return Ok(Value::Null);
                        };
                        let dimensions = array_dimensions(items)?;
                        dimensions.get(index).map_or(Ok(Value::Null), |length| {
                            i64::try_from(*length)
                                .map(Value::Int)
                                .map_err(|_| out_of_range("array length"))
                        })
                    }
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "{name}: not an array {other:?}"
                    ))),
                }
            }
            "array_lower" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_lower takes 2 args".into()));
                }
                match &args[0] {
                    Value::List(items) => {
                        if matches!(args[1], Value::Null) || items.is_empty() {
                            return Ok(Value::Null);
                        }
                        let dimension = to_i64(&args[1])?;
                        if dimension <= 0 {
                            return Ok(Value::Null);
                        }
                        let Ok(index) = usize::try_from(dimension - 1) else {
                            return Ok(Value::Null);
                        };
                        let dimensions = array_dimensions(items)?;
                        Ok(if dimensions.get(index).is_some_and(|length| *length > 0) {
                            Value::Int(1)
                        } else {
                            Value::Null
                        })
                    }
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "array_lower: not an array {other:?}"
                    ))),
                }
            }
            "cardinality" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("cardinality takes 1 arg".into()));
                }
                match &args[0] {
                    Value::List(items) => {
                        let dimensions = array_dimensions(items)?;
                        let cardinality =
                            dimensions.into_iter().try_fold(1_i64, |total, length| {
                                let length = i64::try_from(length)
                                    .map_err(|_| out_of_range("array cardinality"))?;
                                total
                                    .checked_mul(length)
                                    .ok_or_else(|| out_of_range("array cardinality"))
                            })?;
                        Ok(Value::Int(cardinality))
                    }
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "cardinality: not an array {other:?}"
                    ))),
                }
            }
            "array_cat" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_cat takes 2 args".into()));
                }
                match (&args[0], &args[1]) {
                    (Value::List(a), Value::List(b)) => {
                        let mut out = a.clone();
                        out.extend(b.iter().cloned());
                        array_dimensions(&out)?;
                        Ok(Value::List(out))
                    }
                    _ => Err(SQLError::TypeMismatch(
                        "array_cat: both args must be arrays".into(),
                    )),
                }
            }
            "array_append" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_append takes 2 args".into()));
                }
                match &args[0] {
                    Value::List(items) => {
                        let mut out = items.clone();
                        out.push(args[1].clone());
                        array_dimensions(&out)?;
                        Ok(Value::List(out))
                    }
                    Value::Null => Ok(Value::List(vec![args[1].clone()])),
                    other => Err(SQLError::TypeMismatch(format!(
                        "array_append: not an array {other:?}"
                    ))),
                }
            }
            "array_prepend" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_prepend takes 2 args".into()));
                }
                match &args[1] {
                    Value::List(items) => {
                        let mut out = vec![args[0].clone()];
                        out.extend(items.iter().cloned());
                        array_dimensions(&out)?;
                        Ok(Value::List(out))
                    }
                    Value::Null => Ok(Value::List(vec![args[0].clone()])),
                    other => Err(SQLError::TypeMismatch(format!(
                        "array_prepend: not an array {other:?}"
                    ))),
                }
            }
            "array_remove" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_remove takes 2 args".into()));
                }
                match &args[0] {
                    Value::List(items) => Ok(Value::List(
                        items.iter().filter(|v| **v != args[1]).cloned().collect(),
                    )),
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "array_remove: not an array {other:?}"
                    ))),
                }
            }
            "array_position" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_position takes 2 args".into()));
                }
                match &args[0] {
                    Value::List(items) => Ok(items
                        .iter()
                        .position(|v| *v == args[1])
                        .map(|i| Value::Int((i + 1) as i64))
                        .unwrap_or(Value::Null)),
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "array_position: not an array {other:?}"
                    ))),
                }
            }
            "unnest" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("unnest takes 1 arg".into()));
                }
                match &args[0] {
                    Value::List(items) => Ok(Value::List(items.clone())),
                    Value::Null => Ok(Value::List(Vec::new())),
                    other => Err(SQLError::TypeMismatch(format!(
                        "unnest: not an array {other:?}"
                    ))),
                }
            }
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })())
}
