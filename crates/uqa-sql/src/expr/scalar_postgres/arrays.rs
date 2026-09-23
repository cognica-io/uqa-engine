//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable array transforms retain their output and traverse borrowed input under the invoking allowance.

use super::super::{
    binary::values_equal_with_control,
    conversion::{nonnegative_usize, to_i64_with_control, value_to_string_with_control},
    out_of_range, ArrayValue, Result, SQLError, Value,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};

pub(in crate::expr) fn eval_postgres_arrays_with_control(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    if !matches!(
        name,
        "array_positions"
            | "array_replace"
            | "array_to_string"
            | "array_fill"
            | "trim_array"
            | "array_overlap"
            | "contains_op"
            | "contained_by_op"
    ) {
        return None;
    }
    Some((|| {
        control.check()?;
        match name {
            "array_positions" => positions(args, control),
            "array_replace" => replace(args, control),
            "array_to_string" => join(args, control),
            "array_fill" => fill(args, control),
            "trim_array" => trim(args, control),
            "array_overlap" => overlap(args, control),
            "contains_op" | "contained_by_op" => containment(name, args, control),
            _ => unreachable!("array family checked before dispatch"),
        }
    })())
}

fn inline(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn positions(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(
            "array_positions takes 2 args".into(),
        ));
    }
    match &args[0] {
        Value::Array(array) if array.dimensions().len() <= 1 => {
            let lower = i64::from(array.lower_bound(0).unwrap_or(1));
            let mut positions = ProductionVec::new(*control);
            for (index, value) in array.elements().iter().enumerate() {
                if value.cmp_with_control(&args[1], control)?.is_eq() {
                    let position = i64::try_from(index)
                        .ok()
                        .and_then(|index| lower.checked_add(index))
                        .ok_or_else(|| out_of_range("array position"))?;
                    positions.push_produced(inline(Value::Int(position), control)?)?;
                }
            }
            build(positions.finish()?, None, control, "invalid array result")
        }
        Value::Array(_) => Err(SQLError::TypeMismatch(
            "searching for elements in multidimensional arrays is not supported".into(),
        )),
        Value::Null => inline(Value::Null, control),
        other => Err(SQLError::TypeMismatch(format!(
            "array_positions: not an array {other:?}"
        ))),
    }
}

fn replace(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 3 {
        return Err(SQLError::TypeMismatch("array_replace takes 3 args".into()));
    }
    match &args[0] {
        Value::Array(array) => {
            let elements = replace_elements(array.elements(), &args[1], &args[2], control)?;
            build(
                elements,
                Some(bounds(array.lower_bounds(), control)?),
                control,
                "array dimensions do not match",
            )
        }
        Value::Null => inline(Value::Null, control),
        other => Err(SQLError::TypeMismatch(format!(
            "array_replace: not an array {other:?}"
        ))),
    }
}

fn replace_elements(
    elements: &[Value],
    from: &Value,
    to: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(elements.len())?;
    for value in elements {
        let value = match value {
            Value::List(nested) => list(replace_elements(nested, from, to, control)?, control)?,
            value if value.cmp_with_control(from, control)?.is_eq() => control.copy_value(to)?,
            value => control.copy_value(value)?,
        };
        output.push_produced(value)?;
    }
    Ok(output.finish()?)
}

fn join(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if !(2..=3).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            "array_to_string takes 2-3 args".into(),
        ));
    }
    let Value::Array(array) = &args[0] else {
        if matches!(args[0], Value::Null) {
            return inline(Value::Null, control);
        }
        return Err(SQLError::TypeMismatch(format!(
            "array_to_string: not an array {:?}",
            args[0]
        )));
    };
    if matches!(args[1], Value::Null) {
        return inline(Value::Null, control);
    }
    let separator = value_to_string_with_control(&args[1], control)?;
    let null_text = args.get(2).filter(|value| !matches!(value, Value::Null));
    let mut elements = array.elements_with_control(control)?;
    let mut output = ProductionString::new(*control);
    let mut first = true;
    while let Some(value) = elements.next_element()? {
        let value = if matches!(value, Value::Null) {
            let Some(value) = null_text else {
                continue;
            };
            value
        } else {
            value
        };
        if !first {
            output.push_str(&separator)?;
        }
        first = false;
        output.push_str(&value_to_string_with_control(value, control)?)?;
    }
    let (value, memory) = output.finish()?.into_parts();
    Ok(control.finish(Value::Str(value), memory)?)
}

fn fill(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if !(2..=3).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            "array_fill takes 2 or 3 args".into(),
        ));
    }
    if matches!(args[1], Value::Null)
        || args
            .get(2)
            .is_some_and(|value| matches!(value, Value::Null))
    {
        return inline(Value::Null, control);
    }
    let Value::Array(input) = &args[1] else {
        return Err(SQLError::TypeMismatch(
            "array_fill: dimensions must be an integer array".into(),
        ));
    };
    if input.dimensions().len() != 1 {
        return Err(SQLError::TypeMismatch(
            "array_fill: dimensions must be one-dimensional".into(),
        ));
    }
    let mut dimensions = ProductionVec::new(*control);
    for dimension in input.elements() {
        dimensions.push_copy(nonnegative_usize(
            to_i64_with_control(dimension, control)?,
            "array_fill dimension",
        )?)?;
    }
    let dimensions = dimensions.finish()?;
    if dimensions.is_empty() || dimensions.len() > 6 {
        return Err(SQLError::TypeMismatch(
            "array_fill requires between 1 and 6 dimensions".into(),
        ));
    }
    let mut lower_bounds = ProductionVec::new(*control);
    match args.get(2) {
        None => {
            for _ in &*dimensions {
                lower_bounds.push_copy(1)?;
            }
        }
        Some(Value::Array(bounds)) if bounds.dimensions().len() == 1 => {
            for bound in bounds.elements() {
                lower_bounds.push_copy(
                    i32::try_from(to_i64_with_control(bound, control)?)
                        .map_err(|_| out_of_range("array lower bound"))?,
                )?;
            }
        }
        Some(_) => {
            return Err(SQLError::TypeMismatch(
                "array_fill: lower bounds must be a one-dimensional integer array".into(),
            ));
        }
    }
    if lower_bounds.len() != dimensions.len() {
        return Err(SQLError::TypeMismatch(
            "wrong number of array subscripts".into(),
        ));
    }
    if dimensions.contains(&0) {
        return build(
            ProductionVec::new(*control).finish()?,
            None,
            control,
            "invalid empty array",
        );
    }
    let elements = filled_elements(&args[0], &dimensions, control)?;
    build(
        elements,
        Some(lower_bounds.finish()?),
        control,
        "invalid array dimensions",
    )
}

fn filled_elements(
    value: &Value,
    dimensions: &[usize],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(dimensions[0])?;
    if dimensions.len() == 1 {
        for _ in 0..dimensions[0] {
            output.push_produced(control.copy_value(value)?)?;
        }
    } else {
        let nested = list(filled_elements(value, &dimensions[1..], control)?, control)?;
        for _ in 1..dimensions[0] {
            output.push_produced(control.copy_value(&nested)?)?;
        }
        if dimensions[0] != 0 {
            output.push_produced(nested)?;
        }
    }
    Ok(output.finish()?)
}

fn trim(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("trim_array takes 2 args".into()));
    }
    let Value::Array(array) = &args[0] else {
        if matches!(args[0], Value::Null) {
            return inline(Value::Null, control);
        }
        return Err(SQLError::TypeMismatch("trim_array: not an array".into()));
    };
    let count = usize::try_from(to_i64_with_control(&args[1], control)?).ok();
    if count.is_none_or(|count| count > array.elements().len()) {
        return Err(SQLError::Routine {
            sqlstate: "2202E".into(),
            message: format!(
                "number of elements to trim must be between 0 and {}",
                array.elements().len()
            ),
        });
    }
    let count = count.ok_or_else(|| out_of_range("array trim count"))?;
    let mut elements = ProductionVec::new(*control);
    for value in &array.elements()[..array.elements().len() - count] {
        elements.push_produced(control.copy_value(value)?)?;
    }
    let mut lower_bounds = ProductionVec::new(*control);
    for _ in array.lower_bounds() {
        lower_bounds.push_copy(1)?;
    }
    build(
        elements.finish()?,
        Some(lower_bounds.finish()?),
        control,
        "array dimensions do not match",
    )
}

fn overlap(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("array overlap takes 2 args".into()));
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => inline(Value::Null, control),
        (Value::Array(left), Value::Array(right)) => {
            let mut left = left.elements_with_control(control)?;
            while let Some(value) = left.next_element()? {
                if matches!(value, Value::Null) {
                    continue;
                }
                if contains(right, value, control)? {
                    return inline(Value::Bool(true), control);
                }
            }
            inline(Value::Bool(false), control)
        }
        _ => Err(SQLError::TypeMismatch(
            "array overlap: both args must be arrays".into(),
        )),
    }
}

fn contains(array: &ArrayValue, value: &Value, control: &ProductionControl<'_>) -> Result<bool> {
    let mut elements = array.elements_with_control(control)?;
    while let Some(element) = elements.next_element()? {
        if values_equal_with_control(element, value, control)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn containment(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(format!(
            "containment operator takes 2 args, got {}",
            args.len()
        )));
    }
    if matches!(args[0], Value::Null) || matches!(args[1], Value::Null) {
        return inline(Value::Null, control);
    }
    let (left, right) = if name == "contains_op" {
        (&args[0], &args[1])
    } else {
        (&args[1], &args[0])
    };
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => {
            let mut right = right.elements_with_control(control)?;
            while let Some(value) = right.next_element()? {
                if matches!(value, Value::Null) || !contains(left, value, control)? {
                    return inline(Value::Bool(false), control);
                }
            }
            inline(Value::Bool(true), control)
        }
        (Value::JsonB(_), Value::JsonB(_) | Value::Str(_)) | (Value::Str(_), Value::JsonB(_)) => {
            let name = if name == "contains_op" {
                "json_contains"
            } else {
                "json_contained_by"
            };
            super::super::scalar_json::eval_json_functions_with_control(name, args, control)
                .expect("JSON containment family")
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "containment operator requires two arrays or two jsonb values, got {:?} and {:?}",
            args[0], args[1]
        ))),
    }
}

pub(super) fn bounds(
    values: &[i32],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<i32>>> {
    let mut output = ProductionVec::new(*control);
    for value in values {
        output.push_copy(*value)?;
    }
    Ok(output.finish()?)
}

pub(super) fn list(
    elements: Produced<Vec<Value>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (value, memory) = elements.into_parts();
    Ok(control.finish(Value::List(value), memory)?)
}

pub(super) fn build(
    elements: Produced<Vec<Value>>,
    bounds: Option<Produced<Vec<i32>>>,
    control: &ProductionControl<'_>,
    invalid: &str,
) -> Result<Produced<Value>> {
    let output = if elements.is_empty() {
        drop(bounds);
        ArrayValue::try_new_with_control(elements, control)?
    } else if let Some(bounds) = bounds {
        ArrayValue::with_lower_bounds_with_control(elements, bounds, control)?
    } else {
        ArrayValue::try_new_with_control(elements, control)?
    }
    .ok_or_else(|| SQLError::TypeMismatch(invalid.into()))?;
    let (value, memory) = output.into_parts();
    Ok(control.finish(Value::Array(value), memory)?)
}

#[cfg(test)]
mod tests;
