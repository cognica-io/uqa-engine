//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` array-element ordering with fallible composite comparisons.

use super::{ArrayValue, Result, SQLError, Value};
use std::cmp::Ordering;

pub(super) fn sorted_elements(
    array: &ArrayValue,
    descending: bool,
    nulls_first: bool,
    declared_json: bool,
) -> Result<Vec<Value>> {
    let mut elements = array.elements().to_vec();
    if elements.len() > 1
        && array.dimensions().len() <= 1
        && (declared_json
            || elements
                .iter()
                .any(|element| matches!(element, Value::Json(_))))
    {
        return Err(comparison_error("0A000"));
    }
    let mut error = None;
    elements.sort_by(|left, right| {
        if error.is_some() {
            return Ordering::Equal;
        }
        let ordering = match (left, right) {
            (Value::Null, Value::Null) => Ok(Ordering::Equal),
            (Value::Null, _) if nulls_first => Ok(Ordering::Less),
            (Value::Null, _) => Ok(Ordering::Greater),
            (_, Value::Null) if nulls_first => Ok(Ordering::Greater),
            (_, Value::Null) => Ok(Ordering::Less),
            (left, right) => compare_values(left, right).map(|ordering| {
                if descending {
                    ordering.reverse()
                } else {
                    ordering
                }
            }),
        };
        match ordering {
            Ok(ordering) => ordering,
            Err(comparison_error) => {
                error = Some(comparison_error);
                Ordering::Equal
            }
        }
    });
    error.map_or(Ok(elements), Err)
}

fn compare_values(left: &Value, right: &Value) -> Result<Ordering> {
    match (left, right) {
        (Value::Null, Value::Null) => Ok(Ordering::Equal),
        (Value::Null, _) => Ok(Ordering::Greater),
        (_, Value::Null) => Ok(Ordering::Less),
        (Value::Json(_), Value::Json(_)) => Err(comparison_error("42883")),
        (Value::List(left), Value::List(right)) | (Value::Row(left), Value::Row(right)) => {
            compare_slices(left, right)
        }
        (Value::Record(left), Value::Record(right)) => compare_records(left, right),
        (Value::Array(left), Value::Array(right)) => compare_arrays(left, right),
        _ => Ok(left.cmp(right)),
    }
}

fn compare_records(left: &[(String, Value)], right: &[(String, Value)]) -> Result<Ordering> {
    for ((_, left), (_, right)) in left.iter().zip(right) {
        let ordering = compare_values(left, right)?;
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

fn compare_slices(left: &[Value], right: &[Value]) -> Result<Ordering> {
    for (left, right) in left.iter().zip(right) {
        let ordering = compare_values(left, right)?;
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

fn compare_arrays(left: &ArrayValue, right: &ArrayValue) -> Result<Ordering> {
    let ordering = compare_slices(left.elements(), right.elements())?;
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
