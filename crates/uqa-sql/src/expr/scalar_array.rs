//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core `PostgreSQL` array built-ins.

use super::{out_of_range, to_i64, ArrayValue, Result, SQLError, Value};

pub(super) fn eval_array_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    const NAMES: &[&str] = &[
        "array_length",
        "array_upper",
        "array_lower",
        "array_dims",
        "array_ndims",
        "cardinality",
        "array_cat",
        "array_append",
        "array_prepend",
        "array_remove",
        "array_position",
        "array_reverse",
        "array_sort",
        "unnest",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Value> {
        match name {
            "array_length" | "array_upper" | "array_lower" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch(format!("{name} takes 2 args")));
                }
                let array = match &args[0] {
                    Value::Array(array) => array,
                    Value::Null => return Ok(Value::Null),
                    other => return Err(not_an_array(name, other)),
                };
                if matches!(args[1], Value::Null) {
                    return Ok(Value::Null);
                }
                let Some(dimension) = dimension_index(&args[1])? else {
                    return Ok(Value::Null);
                };
                let Some(length) = array.dimensions().get(dimension) else {
                    return Ok(Value::Null);
                };
                if *length == 0 {
                    return Ok(Value::Null);
                }
                match name {
                    "array_length" => i64::try_from(*length)
                        .map(Value::Int)
                        .map_err(|_| out_of_range("array length")),
                    "array_lower" => array
                        .lower_bound(dimension)
                        .map(|bound| Value::Int(i64::from(bound)))
                        .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into())),
                    "array_upper" => Ok(array
                        .upper_bound(dimension)
                        .map(Value::Int)
                        .unwrap_or(Value::Null)),
                    _ => unreachable!(),
                }
            }
            "array_dims" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("array_dims takes 1 arg".into()));
                }
                match &args[0] {
                    Value::Array(array) if array.dimensions().is_empty() => Ok(Value::Null),
                    Value::Array(array) => Ok(Value::Str(
                        array
                            .lower_bounds()
                            .iter()
                            .zip(array.dimensions())
                            .map(|(lower, length)| {
                                let length = i64::try_from(*length)
                                    .map_err(|_| out_of_range("array dimension"))?;
                                Ok(format!("[{lower}:{}]", i64::from(*lower) + length - 1))
                            })
                            .collect::<Result<String>>()?,
                    )),
                    Value::Null => Ok(Value::Null),
                    other => Err(not_an_array("array_dims", other)),
                }
            }
            "array_ndims" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("array_ndims takes 1 arg".into()));
                }
                match &args[0] {
                    Value::Array(array) if array.dimensions().is_empty() => Ok(Value::Null),
                    Value::Array(array) => i64::try_from(array.dimensions().len())
                        .map(Value::Int)
                        .map_err(|_| out_of_range("array dimensions")),
                    Value::Null => Ok(Value::Null),
                    other => Err(not_an_array("array_ndims", other)),
                }
            }
            "cardinality" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("cardinality takes 1 arg".into()));
                }
                match &args[0] {
                    Value::Array(array) => {
                        let cardinality = array.dimensions().iter().try_fold(
                            i64::from(!array.dimensions().is_empty()),
                            |total, length| {
                                let length = i64::try_from(*length)
                                    .map_err(|_| out_of_range("array cardinality"))?;
                                total
                                    .checked_mul(length)
                                    .ok_or_else(|| out_of_range("array cardinality"))
                            },
                        )?;
                        Ok(Value::Int(cardinality))
                    }
                    Value::Null => Ok(Value::Null),
                    other => Err(not_an_array("cardinality", other)),
                }
            }
            "array_cat" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_cat takes 2 args".into()));
                }
                match (&args[0], &args[1]) {
                    (Value::Null, Value::Null) => Ok(Value::Null),
                    (Value::Null, Value::Array(array)) | (Value::Array(array), Value::Null) => {
                        Ok(Value::Array(array.clone()))
                    }
                    (Value::Array(left), Value::Array(right)) => concatenate(left, right),
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
                    Value::Array(array) if array.dimensions().len() <= 1 => {
                        let mut elements = array.elements().to_vec();
                        elements.push(args[1].clone());
                        rebuild_array(array, elements)
                    }
                    Value::Array(_) => Err(SQLError::TypeMismatch(
                        "argument must be an empty or one-dimensional array".into(),
                    )),
                    Value::Null => ArrayValue::try_new(vec![args[1].clone()])
                        .map(Value::Array)
                        .ok_or_else(|| SQLError::TypeMismatch("invalid array element".into())),
                    other => Err(not_an_array("array_append", other)),
                }
            }
            "array_prepend" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_prepend takes 2 args".into()));
                }
                match &args[1] {
                    Value::Array(array) if array.dimensions().len() <= 1 => {
                        let mut elements = Vec::with_capacity(array.elements().len() + 1);
                        elements.push(args[0].clone());
                        elements.extend(array.elements().iter().cloned());
                        rebuild_array(array, elements)
                    }
                    Value::Array(_) => Err(SQLError::TypeMismatch(
                        "argument must be an empty or one-dimensional array".into(),
                    )),
                    Value::Null => ArrayValue::try_new(vec![args[0].clone()])
                        .map(Value::Array)
                        .ok_or_else(|| SQLError::TypeMismatch("invalid array element".into())),
                    other => Err(not_an_array("array_prepend", other)),
                }
            }
            "array_remove" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_remove takes 2 args".into()));
                }
                match &args[0] {
                    Value::Array(array) if array.dimensions().len() <= 1 => rebuild_array(
                        array,
                        array
                            .elements()
                            .iter()
                            .filter(|value| **value != args[1])
                            .cloned()
                            .collect(),
                    ),
                    Value::Array(_) => Err(SQLError::TypeMismatch(
                        "removing elements from multidimensional arrays is not supported".into(),
                    )),
                    Value::Null => Ok(Value::Null),
                    other => Err(not_an_array("array_remove", other)),
                }
            }
            "array_position" => {
                if !(2..=3).contains(&args.len()) {
                    return Err(SQLError::TypeMismatch(
                        "array_position takes 2 or 3 args".into(),
                    ));
                }
                match &args[0] {
                    Value::Array(array) if array.dimensions().len() <= 1 => {
                        let lower = i64::from(array.lower_bound(0).unwrap_or(1));
                        let start = match args.get(2) {
                            Some(Value::Null) => return Ok(Value::Null),
                            Some(value) => to_i64(value)?,
                            None => lower,
                        };
                        let offset = usize::try_from(start.saturating_sub(lower).max(0))
                            .map_err(|_| out_of_range("array position"))?;
                        Ok(array
                            .elements()
                            .iter()
                            .enumerate()
                            .skip(offset)
                            .find(|(_, value)| **value == args[1])
                            .and_then(|(index, _)| i64::try_from(index).ok())
                            .and_then(|index| lower.checked_add(index))
                            .map(Value::Int)
                            .unwrap_or(Value::Null))
                    }
                    Value::Array(_) => Err(SQLError::TypeMismatch(
                        "searching for elements in multidimensional arrays is not supported".into(),
                    )),
                    Value::Null => Ok(Value::Null),
                    other => Err(not_an_array("array_position", other)),
                }
            }
            "array_reverse" | "array_sort" => {
                if name == "array_reverse" && args.len() != 1 {
                    return Err(SQLError::TypeMismatch("array_reverse takes 1 arg".into()));
                }
                if name == "array_sort" && !(1..=3).contains(&args.len()) {
                    return Err(SQLError::TypeMismatch(
                        "array_sort takes 1 to 3 args".into(),
                    ));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let Value::Array(array) = &args[0] else {
                    return Err(not_an_array(name, &args[0]));
                };
                let mut elements = array.elements().to_vec();
                if name == "array_reverse" {
                    elements.reverse();
                    return rebuild_array(array, elements);
                }
                let descending =
                    boolean_option(args.get(1), "array_sort: descending")?.unwrap_or(false);
                let nulls_first =
                    boolean_option(args.get(2), "array_sort: nulls_first")?.unwrap_or(descending);
                elements.sort_by(|left, right| match (left, right) {
                    (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
                    (Value::Null, _) if nulls_first => std::cmp::Ordering::Less,
                    (Value::Null, _) => std::cmp::Ordering::Greater,
                    (_, Value::Null) if nulls_first => std::cmp::Ordering::Greater,
                    (_, Value::Null) => std::cmp::Ordering::Less,
                    (left, right) if descending => right.cmp(left),
                    (left, right) => left.cmp(right),
                });
                rebuild_array(array, elements)
            }
            "unnest" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("unnest takes 1 arg".into()));
                }
                match &args[0] {
                    Value::Array(array) => {
                        let mut values = Vec::new();
                        flatten_elements(array.elements(), &mut values);
                        Ok(Value::List(values))
                    }
                    Value::Null => Ok(Value::List(Vec::new())),
                    other => Err(not_an_array("unnest", other)),
                }
            }
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })())
}

fn dimension_index(value: &Value) -> Result<Option<usize>> {
    let dimension = to_i64(value)?;
    if dimension <= 0 {
        return Ok(None);
    }
    Ok(usize::try_from(dimension - 1).ok())
}

fn boolean_option(value: Option<&Value>, label: &str) -> Result<Option<bool>> {
    match value {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(SQLError::TypeMismatch(format!(
            "{label} must be boolean, got {other:?}"
        ))),
    }
}

fn rebuild_array(original: &ArrayValue, elements: Vec<Value>) -> Result<Value> {
    let rebuilt = if elements.is_empty() || original.dimensions().is_empty() {
        ArrayValue::try_new(elements)
    } else {
        ArrayValue::with_lower_bounds(elements, original.lower_bounds().to_vec())
    };
    rebuilt
        .map(Value::Array)
        .ok_or_else(|| SQLError::TypeMismatch("array dimensions do not match".into()))
}

fn concatenate(left: &ArrayValue, right: &ArrayValue) -> Result<Value> {
    if left.dimensions().is_empty() {
        return Ok(Value::Array(right.clone()));
    }
    if right.dimensions().is_empty() {
        return Ok(Value::Array(left.clone()));
    }
    let (elements, lower_bounds) = if left.dimensions().len() == right.dimensions().len() {
        if left.dimensions().get(1..) != right.dimensions().get(1..)
            || left.lower_bounds().get(1..) != right.lower_bounds().get(1..)
        {
            return Err(incompatible_array_concat());
        }
        let mut elements = left.elements().to_vec();
        elements.extend(right.elements().iter().cloned());
        (elements, left.lower_bounds().to_vec())
    } else if left.dimensions().len() + 1 == right.dimensions().len() {
        if left.dimensions() != &right.dimensions()[1..]
            || left.lower_bounds() != &right.lower_bounds()[1..]
        {
            return Err(incompatible_array_concat());
        }
        let mut elements = vec![Value::List(left.elements().to_vec())];
        elements.extend(right.elements().iter().cloned());
        (elements, right.lower_bounds().to_vec())
    } else if left.dimensions().len() == right.dimensions().len() + 1 {
        if &left.dimensions()[1..] != right.dimensions()
            || &left.lower_bounds()[1..] != right.lower_bounds()
        {
            return Err(incompatible_array_concat());
        }
        let mut elements = left.elements().to_vec();
        elements.push(Value::List(right.elements().to_vec()));
        (elements, left.lower_bounds().to_vec())
    } else {
        return Err(incompatible_array_concat());
    };
    ArrayValue::with_lower_bounds(elements, lower_bounds)
        .map(Value::Array)
        .ok_or_else(incompatible_array_concat)
}

fn incompatible_array_concat() -> SQLError {
    SQLError::Routine {
        sqlstate: "2202E".into(),
        message: "cannot concatenate incompatible arrays".into(),
    }
}

fn flatten_elements(elements: &[Value], output: &mut Vec<Value>) {
    for element in elements {
        if let Value::List(nested) = element {
            flatten_elements(nested, output);
        } else {
            output.push(element.clone());
        }
    }
}

fn not_an_array(function: &str, value: &Value) -> SQLError {
    SQLError::TypeMismatch(format!("{function}: not an array {value:?}"))
}
