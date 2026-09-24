//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` array-element ordering with fallible composite comparisons.

use super::{
    copy_elements, ArrayValue, Produced, ProductionControl, ProductionVec, Result, SQLError, Value,
};
use std::cmp::Ordering;
use uqa_core::ordering::sort_by_with_control;

pub(super) fn sorted_elements(
    array: &ArrayValue,
    descending: bool,
    nulls_first: bool,
    declared_json: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    if array.elements().len() > 1
        && array.dimensions().len() <= 1
        && (declared_json
            || array
                .elements()
                .iter()
                .any(|element| matches!(element, Value::Json(_))))
    {
        return Err(comparison_error("0A000"));
    }
    let mut order = ProductionVec::new(*control);
    for (index, value) in array.elements().iter().enumerate() {
        order.push_copy((index, value))?;
    }
    let mut order = order.finish()?;
    sort_by_with_control(
        order.as_mut_slice(),
        &mut || control.check().map_err(SQLError::from),
        |(left_index, left), (right_index, right), _| {
            let ordering = match (left, right) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) if nulls_first => Ordering::Less,
                (Value::Null, _) => Ordering::Greater,
                (_, Value::Null) if nulls_first => Ordering::Greater,
                (_, Value::Null) => Ordering::Less,
                (left, right) => {
                    let order = compare_values(left, right, control)?;
                    if descending {
                        order.reverse()
                    } else {
                        order
                    }
                }
            };
            // Match the original stable sort without allocating a second owned array or sort buffer.
            Ok(ordering.then_with(|| left_index.cmp(right_index)))
        },
    )?;
    copy_elements(order.iter().map(|(_, value)| *value), control)
}

fn compare_values(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    control.check()?;
    match (left, right) {
        (Value::Null, Value::Null) => Ok(Ordering::Equal),
        (Value::Null, _) => Ok(Ordering::Greater),
        (_, Value::Null) => Ok(Ordering::Less),
        (Value::Json(_), Value::Json(_)) => Err(comparison_error("42883")),
        (Value::List(left), Value::List(right)) | (Value::Row(left), Value::Row(right)) => {
            compare_slices(left, right, control)
        }
        (Value::Record(left), Value::Record(right)) => compare_records(left, right, control),
        (Value::Array(left), Value::Array(right)) => compare_arrays(left, right, control),
        _ => Ok(left.cmp_with_control(right, control)?),
    }
}

fn compare_records(
    left: &[(String, Value)],
    right: &[(String, Value)],
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    for ((_, left), (_, right)) in left.iter().zip(right) {
        let ordering = compare_values(left, right, control)?;
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

fn compare_slices(
    left: &[Value],
    right: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    for (left, right) in left.iter().zip(right) {
        let ordering = compare_values(left, right, control)?;
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

fn compare_arrays(
    left: &ArrayValue,
    right: &ArrayValue,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    let ordering = compare_slices(left.elements(), right.elements(), control)?;
    if ordering != Ordering::Equal {
        return Ok(ordering);
    }
    Ok(left
        .dimensions()
        .len()
        .cmp(&right.dimensions().len())
        .then_with(|| left.dimensions().cmp(right.dimensions()))
        .then_with(|| left.lower_bounds().cmp(right.lower_bounds())))
}

fn comparison_error(sqlstate: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: "could not identify a comparison function for type json".into(),
    }
}
