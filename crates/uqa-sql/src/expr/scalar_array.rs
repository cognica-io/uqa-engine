//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core `PostgreSQL` array built-ins share admitted result constructors.

use super::conversion::to_i64_with_control;
use super::{out_of_range, ArrayValue, Result, SQLError, Value};
use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};

mod order;
mod properties;

pub(super) fn eval_array_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    eval_array_functions_with_control(name, args, &ProductionControl::uncontrolled())
        .map(|result| result.map(ordinary))
}

pub(super) fn eval_array_functions_with_control(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
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
    NAMES
        .contains(&name)
        .then(|| eval_array_function(name, args, false, control))
}

pub(super) fn eval_dispatched_json_array_sort(args: &[Value]) -> Result<Value> {
    eval_dispatched_json_array_sort_with_control(args, &ProductionControl::uncontrolled())
        .map(ordinary)
}

pub(super) fn eval_dispatched_json_array_sort_with_control(
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    eval_array_function("array_sort", args, true, control)
}

fn ordinary(value: Produced<Value>) -> Value {
    value.into_uncontrolled().expect("ordinary array result")
}

fn inline(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn eval_array_function(
    name: &str,
    args: &[Value],
    json_sort: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    match name {
        "array_length" | "array_upper" | "array_lower" | "array_ndims" | "cardinality" => {
            inline(properties::evaluate(name, args, control)?, control)
        }
        "array_dims" => dimensions(args, control),
        "array_cat" => {
            require_arity(name, args, 2)?;
            match (&args[0], &args[1]) {
                (Value::Null, Value::Null) => inline(Value::Null, control),
                (Value::Null, Value::Array(_)) => Ok(control.copy_value(&args[1])?),
                (Value::Array(_), Value::Null) => Ok(control.copy_value(&args[0])?),
                (Value::Array(left), Value::Array(right)) => concatenate(left, right, control),
                _ => Err(SQLError::TypeMismatch(
                    "array_cat: both args must be arrays".into(),
                )),
            }
        }
        "array_append" | "array_prepend" => append(name, args, control),
        "array_remove" => remove(args, control),
        "array_position" => position(args, control),
        "array_reverse" | "array_sort" => reordered(name, args, json_sort, control),
        "unnest" => {
            require_arity(name, args, 1)?;
            let mut values = ProductionVec::new(*control);
            match &args[0] {
                Value::Array(array) => flatten_elements(array.elements(), &mut values, control)?,
                Value::Null => {}
                other => return Err(not_an_array(name, other)),
            }
            list(values.finish()?, control)
        }
        _ => unreachable!("function family membership was checked before dispatch"),
    }
}

fn require_arity(name: &str, args: &[Value], count: usize) -> Result<()> {
    if args.len() == count {
        Ok(())
    } else {
        Err(SQLError::TypeMismatch(format!(
            "{name} takes {count} {}",
            if count == 1 { "arg" } else { "args" }
        )))
    }
}

fn dimensions(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    require_arity("array_dims", args, 1)?;
    let array = match &args[0] {
        Value::Null => return inline(Value::Null, control),
        Value::Array(array) if array.dimensions().is_empty() => {
            return inline(Value::Null, control)
        }
        Value::Array(array) => array,
        other => return Err(not_an_array("array_dims", other)),
    };
    let mut output = ProductionString::new(*control);
    for (lower, length) in array.lower_bounds().iter().zip(array.dimensions()) {
        let length = i64::try_from(*length).map_err(|_| out_of_range("array dimension"))?;
        output.push_str(
            &control.format(format_args!("[{lower}:{}]", i64::from(*lower) + length - 1))?,
        )?;
    }
    let (text, memory) = output.finish()?.into_parts();
    Ok(control.finish(Value::Str(text), memory)?)
}

fn append(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    require_arity(name, args, 2)?;
    let prepend = name == "array_prepend";
    let (source, item) = if prepend {
        (&args[1], &args[0])
    } else {
        (&args[0], &args[1])
    };
    let array = match source {
        Value::Array(array) if array.dimensions().len() <= 1 => Some(array),
        Value::Array(_) => {
            return Err(SQLError::TypeMismatch(
                "argument must be an empty or one-dimensional array".into(),
            ))
        }
        Value::Null => None,
        other => return Err(not_an_array(name, other)),
    };
    let mut elements = ProductionVec::new(*control);
    if prepend {
        elements.push_produced(control.copy_value(item)?)?;
    }
    if let Some(array) = array {
        copy_into(array.elements().iter(), &mut elements, control)?;
    }
    if !prepend {
        elements.push_produced(control.copy_value(item)?)?;
    }
    let elements = elements.finish()?;
    match array {
        Some(array) => rebuild_array(array, elements, control),
        None => finish_array(
            ArrayValue::try_new_with_control(elements, control)?,
            control,
            || SQLError::TypeMismatch("invalid array element".into()),
        ),
    }
}

fn remove(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    require_arity("array_remove", args, 2)?;
    let array = match &args[0] {
        Value::Null => return inline(Value::Null, control),
        Value::Array(array) if array.dimensions().len() <= 1 => array,
        Value::Array(_) => {
            return Err(SQLError::TypeMismatch(
                "removing elements from multidimensional arrays is not supported".into(),
            ))
        }
        other => return Err(not_an_array("array_remove", other)),
    };
    let mut elements = ProductionVec::new(*control);
    for value in array.elements() {
        if !value.cmp_with_control(&args[1], control)?.is_eq() {
            elements.push_produced(control.copy_value(value)?)?;
        }
    }
    rebuild_array(array, elements.finish()?, control)
}

fn position(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if !(2..=3).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            "array_position takes 2 or 3 args".into(),
        ));
    }
    let array = match &args[0] {
        Value::Null => return inline(Value::Null, control),
        Value::Array(array) if array.dimensions().len() <= 1 => array,
        Value::Array(_) => {
            return Err(SQLError::TypeMismatch(
                "searching for elements in multidimensional arrays is not supported".into(),
            ))
        }
        other => return Err(not_an_array("array_position", other)),
    };
    let lower = i64::from(array.lower_bound(0).unwrap_or(1));
    let start = match args.get(2) {
        Some(Value::Null) => return inline(Value::Null, control),
        Some(value) => to_i64_with_control(value, control)?,
        None => lower,
    };
    let offset = usize::try_from(start.saturating_sub(lower).max(0))
        .map_err(|_| out_of_range("array position"))?;
    for (index, value) in array.elements().iter().enumerate().skip(offset) {
        if value.cmp_with_control(&args[1], control)?.is_eq() {
            return inline(
                i64::try_from(index)
                    .ok()
                    .and_then(|index| lower.checked_add(index))
                    .map(Value::Int)
                    .unwrap_or(Value::Null),
                control,
            );
        }
    }
    inline(Value::Null, control)
}

fn reordered(
    name: &str,
    args: &[Value],
    json_sort: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if name == "array_reverse" {
        require_arity(name, args, 1)?;
    } else if !(1..=3).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            "array_sort takes 1 to 3 args".into(),
        ));
    }
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return inline(Value::Null, control);
    }
    let Value::Array(array) = &args[0] else {
        return Err(not_an_array(name, &args[0]));
    };
    let elements = if name == "array_reverse" {
        copy_elements(array.elements().iter().rev(), control)?
    } else {
        let descending = boolean_option(args.get(1), "array_sort: descending")?.unwrap_or(false);
        let nulls_first =
            boolean_option(args.get(2), "array_sort: nulls_first")?.unwrap_or(descending);
        order::sorted_elements(array, descending, nulls_first, json_sort, control)?
    };
    rebuild_array(array, elements, control)
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

fn copy_into<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    output: &mut ProductionVec<'_, Value>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    for value in values {
        output.push_produced(control.copy_value(value)?)?;
    }
    Ok(())
}

fn copy_elements<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    let mut output = ProductionVec::new(*control);
    copy_into(values, &mut output, control)?;
    Ok(output.finish()?)
}

fn bounds(values: &[i32], control: &ProductionControl<'_>) -> Result<Produced<Vec<i32>>> {
    let mut output = ProductionVec::new(*control);
    for value in values {
        output.push_copy(*value)?;
    }
    Ok(output.finish()?)
}

fn list(
    elements: Produced<Vec<Value>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (elements, memory) = elements.into_parts();
    Ok(control.finish(Value::List(elements), memory)?)
}

fn finish_array(
    array: Option<Produced<ArrayValue>>,
    control: &ProductionControl<'_>,
    invalid: impl FnOnce() -> SQLError,
) -> Result<Produced<Value>> {
    let (array, memory) = array.ok_or_else(invalid)?.into_parts();
    Ok(control.finish(Value::Array(array), memory)?)
}

fn rebuild_array(
    original: &ArrayValue,
    elements: Produced<Vec<Value>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let rebuilt = if elements.is_empty() || original.dimensions().is_empty() {
        ArrayValue::try_new_with_control(elements, control)?
    } else {
        ArrayValue::with_lower_bounds_with_control(
            elements,
            bounds(original.lower_bounds(), control)?,
            control,
        )?
    };
    finish_array(rebuilt, control, || {
        SQLError::TypeMismatch("array dimensions do not match".into())
    })
}

fn concatenate(
    left: &ArrayValue,
    right: &ArrayValue,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if left.dimensions().is_empty() {
        return rebuild_array(right, copy_elements(right.elements(), control)?, control);
    }
    if right.dimensions().is_empty() {
        return rebuild_array(left, copy_elements(left.elements(), control)?, control);
    }
    let mut elements = ProductionVec::new(*control);
    let lower_bounds = if left.dimensions().len() == right.dimensions().len() {
        if left.dimensions().get(1..) != right.dimensions().get(1..)
            || left.lower_bounds().get(1..) != right.lower_bounds().get(1..)
        {
            return Err(incompatible_array_concat());
        }
        copy_into(left.elements(), &mut elements, control)?;
        copy_into(right.elements(), &mut elements, control)?;
        left.lower_bounds()
    } else if left.dimensions().len() + 1 == right.dimensions().len() {
        if left.dimensions() != &right.dimensions()[1..]
            || left.lower_bounds() != &right.lower_bounds()[1..]
        {
            return Err(incompatible_array_concat());
        }
        elements.push_produced(list(copy_elements(left.elements(), control)?, control)?)?;
        copy_into(right.elements(), &mut elements, control)?;
        right.lower_bounds()
    } else if left.dimensions().len() == right.dimensions().len() + 1 {
        if &left.dimensions()[1..] != right.dimensions()
            || &left.lower_bounds()[1..] != right.lower_bounds()
        {
            return Err(incompatible_array_concat());
        }
        copy_into(left.elements(), &mut elements, control)?;
        elements.push_produced(list(copy_elements(right.elements(), control)?, control)?)?;
        left.lower_bounds()
    } else {
        return Err(incompatible_array_concat());
    };
    finish_array(
        ArrayValue::with_lower_bounds_with_control(
            elements.finish()?,
            bounds(lower_bounds, control)?,
            control,
        )?,
        control,
        incompatible_array_concat,
    )
}

fn incompatible_array_concat() -> SQLError {
    SQLError::Routine {
        sqlstate: "2202E".into(),
        message: "cannot concatenate incompatible arrays".into(),
    }
}

fn flatten_elements(
    elements: &[Value],
    output: &mut ProductionVec<'_, Value>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    for element in elements {
        control.check()?;
        if let Value::List(nested) = element {
            flatten_elements(nested, output, control)?;
        } else {
            output.push_produced(control.copy_value(element)?)?;
        }
    }
    Ok(())
}

fn not_an_array(function: &str, value: &Value) -> SQLError {
    SQLError::TypeMismatch(format!("{function}: not an array {value:?}"))
}

#[cfg(test)]
mod tests;
