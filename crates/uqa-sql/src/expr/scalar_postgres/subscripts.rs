//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL subscripts borrow selected input and retain only admitted copies of the result.

use super::super::{
    conversion::{to_i64_with_control, value_to_string_with_control},
    out_of_range, ArrayValue, Result, SQLError, Value,
};
use super::{
    arrays::{bounds, build, list},
    FunctionDispatch,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(in crate::expr) fn eval_postgres_subscript_with_control(
    dispatch: FunctionDispatch,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    let evaluate = match dispatch {
        FunctionDispatch::ArraySubscripts => eval_array_subscripts,
        FunctionDispatch::ArraySlices => eval_array_slices,
        FunctionDispatch::Subscript => eval_subscript,
        FunctionDispatch::Slice => eval_slice,
        _ => return None,
    };
    Some((|| {
        control.check()?;
        evaluate(args, control)
    })())
}

fn inline(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn empty(control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    build(
        ProductionVec::new(*control).finish()?,
        None,
        control,
        "invalid empty array",
    )
}

fn copied(elements: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Vec<Value>>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(elements.len())?;
    for value in elements {
        output.push_produced(control.copy_value(value)?)?;
    }
    Ok(output.finish()?)
}

fn unit_bounds(count: usize, control: &ProductionControl<'_>) -> Result<Produced<Vec<i32>>> {
    let mut output = ProductionVec::new(*control);
    for _ in 0..count {
        output.push_copy(1)?;
    }
    Ok(output.finish()?)
}

fn eval_array_subscripts(
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if args.len() < 2 {
        return Err(SQLError::TypeMismatch(
            "array subscripting requires at least one index".into(),
        ));
    }
    if args.iter().any(|argument| matches!(argument, Value::Null)) {
        return inline(Value::Null, control);
    }
    let Value::Array(array) = &args[0] else {
        return Err(SQLError::TypeMismatch(format!(
            "cannot subscript {:?}",
            args[0]
        )));
    };
    array_subscripts(array, &args[1..], control)
}

fn eval_array_slices(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() < 3 || args.len().is_multiple_of(2) {
        return Err(SQLError::TypeMismatch(
            "array slicing requires lower/upper bound pairs".into(),
        ));
    }
    let Value::Array(array) = &args[0] else {
        if matches!(args[0], Value::Null) {
            return inline(Value::Null, control);
        }
        return Err(SQLError::TypeMismatch(format!(
            "cannot slice {:?}",
            args[0]
        )));
    };
    array_slices(array, &args[1..], control)
}

fn eval_subscript(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("subscript takes 2 args".into()));
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => inline(Value::Null, control),
        (Value::Array(array), index) => {
            let index = to_i64_with_control(index, control)?;
            let Some(lower) = array.lower_bound(0).map(i64::from) else {
                return inline(Value::Null, control);
            };
            let Some(offset) = index
                .checked_sub(lower)
                .and_then(|offset| usize::try_from(offset).ok())
            else {
                return inline(Value::Null, control);
            };
            let Some(value) = array.elements().get(offset) else {
                return inline(Value::Null, control);
            };
            if array.dimensions().len() == 1 {
                return Ok(control.copy_value(value)?);
            }
            let Value::List(elements) = value else {
                return Err(SQLError::TypeMismatch(
                    "invalid multidimensional array".into(),
                ));
            };
            build(
                copied(elements, control)?,
                Some(bounds(&array.lower_bounds()[1..], control)?),
                control,
                "invalid multidimensional array",
            )
        }
        (Value::Map(map), key) => {
            let key = value_to_string_with_control(key, control)?;
            Ok(control.copy_value(map.get(&*key).unwrap_or(&Value::Null))?)
        }
        (other, _) => Err(SQLError::TypeMismatch(format!(
            "cannot subscript {other:?}"
        ))),
    }
}

fn eval_slice(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 3 {
        return Err(SQLError::TypeMismatch("slice takes 3 args".into()));
    }
    match &args[0] {
        Value::Null => inline(Value::Null, control),
        Value::Array(array) => {
            let Some(array_lower) = array.lower_bound(0).map(i64::from) else {
                return empty(control);
            };
            let array_upper = array
                .upper_bound(0)
                .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into()))?;
            let lo = match &args[1] {
                Value::Null => array_lower,
                other => to_i64_with_control(other, control)?,
            }
            .max(array_lower);
            let hi = match &args[2] {
                Value::Null => array_upper,
                other => to_i64_with_control(other, control)?,
            }
            .min(array_upper);
            if hi < lo || lo > array_upper {
                return empty(control);
            }
            let start =
                usize::try_from(lo - array_lower).map_err(|_| out_of_range("array slice"))?;
            let end =
                usize::try_from(hi - array_lower + 1).map_err(|_| out_of_range("array slice"))?;
            build(
                copied(&array.elements()[start..end], control)?,
                Some(unit_bounds(array.lower_bounds().len(), control)?),
                control,
                "invalid array slice",
            )
        }
        other => Err(SQLError::TypeMismatch(format!("cannot slice {other:?}"))),
    }
}

fn array_subscripts(
    array: &ArrayValue,
    indices: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if indices.len() != array.dimensions().len() || indices.is_empty() {
        return inline(Value::Null, control);
    }
    let mut elements = array.elements();
    for (dimension, index) in indices.iter().enumerate() {
        let lower = i64::from(
            array
                .lower_bound(dimension)
                .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into()))?,
        );
        let index = to_i64_with_control(index, control)?;
        let Some(offset) = index
            .checked_sub(lower)
            .and_then(|offset| usize::try_from(offset).ok())
        else {
            return inline(Value::Null, control);
        };
        let Some(value) = elements.get(offset) else {
            return inline(Value::Null, control);
        };
        if dimension + 1 == indices.len() {
            return Ok(control.copy_value(value)?);
        }
        let Value::List(nested) = value else {
            return Err(SQLError::TypeMismatch(
                "invalid multidimensional array".into(),
            ));
        };
        elements = nested;
    }
    inline(Value::Null, control)
}

fn array_slices(
    array: &ArrayValue,
    bounds: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let supplied_dimensions = bounds.len() / 2;
    if supplied_dimensions > array.dimensions().len() || array.dimensions().is_empty() {
        return empty(control);
    }
    let mut ranges = ProductionVec::new(*control);
    ranges.reserve(array.dimensions().len())?;
    for dimension in 0..array.dimensions().len() {
        let array_lower = i64::from(
            array
                .lower_bound(dimension)
                .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into()))?,
        );
        let array_upper = array
            .upper_bound(dimension)
            .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into()))?;
        let (requested_lower, requested_upper) = if dimension < supplied_dimensions {
            let lower = match &bounds[dimension * 2] {
                Value::Null => array_lower,
                value => to_i64_with_control(value, control)?,
            };
            let upper = match &bounds[dimension * 2 + 1] {
                Value::Null => array_upper,
                value => to_i64_with_control(value, control)?,
            };
            (lower, upper)
        } else {
            (array_lower, array_upper)
        };
        let lower = requested_lower.max(array_lower);
        let upper = requested_upper.min(array_upper);
        if upper < lower {
            return empty(control);
        }
        ranges.push_copy((lower, upper, array_lower))?;
    }
    let elements = slice_array_elements(array.elements(), &ranges, 0, control)?;
    build(
        elements,
        Some(unit_bounds(array.dimensions().len(), control)?),
        control,
        "array dimensions do not match",
    )
}

fn slice_array_elements(
    elements: &[Value],
    ranges: &[(i64, i64, i64)],
    dimension: usize,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    control.check()?;
    let (lower, upper, array_lower) = ranges[dimension];
    let start = usize::try_from(lower - array_lower).map_err(|_| out_of_range("array slice"))?;
    let end = usize::try_from(upper - array_lower + 1).map_err(|_| out_of_range("array slice"))?;
    let selected = elements
        .get(start..end)
        .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into()))?;
    if dimension + 1 == ranges.len() {
        return copied(selected, control);
    }
    let mut output = ProductionVec::new(*control);
    output.reserve(selected.len())?;
    for value in selected {
        let Value::List(nested) = value else {
            return Err(SQLError::TypeMismatch(
                "invalid multidimensional array".into(),
            ));
        };
        output.push_produced(list(
            slice_array_elements(nested, ranges, dimension + 1, control)?,
            control,
        )?)?;
    }
    Ok(output.finish()?)
}

#[cfg(test)]
mod tests;
