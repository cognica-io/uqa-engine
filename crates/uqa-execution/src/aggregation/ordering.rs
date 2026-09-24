//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregate ordering preserves the selected SQL operators and their failures across runs.

use std::cmp::Ordering;
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::SQLError;

pub fn compare_extrema(left: &Value, right: &Value) -> Result<Ordering, SQLError> {
    // PostgreSQL MIN/MAX select array_smaller/array_larger for catalog vectors, independently of oidvector's scalar operators.
    if let (Value::LegacyVector(left), Value::LegacyVector(right)) = (left, right) {
        return Ok(left.compare_as_array(right));
    }
    uqa_sql::expr::compare_typed_values_with_control(
        left,
        right,
        &ProductionControl::uncontrolled(),
    )
}

pub(super) fn compare_sort_keys(
    left: &[(Value, bool)],
    right: &[(Value, bool)],
) -> Result<Ordering, SQLError> {
    for ((left, descending), (right, _)) in left.iter().zip(right) {
        let ordering = uqa_sql::expr::compare_typed_values_with_control(
            left,
            right,
            &ProductionControl::uncontrolled(),
        )?;
        let ordering = if *descending {
            ordering.reverse()
        } else {
            ordering
        };
        if !ordering.is_eq() {
            return Ok(ordering);
        }
    }
    Ok(Ordering::Equal)
}

pub(super) fn sort_records<T>(
    rows: &mut [T],
    compare: impl Fn(&T, &T) -> Result<Ordering, SQLError>,
) -> Result<(), SQLError> {
    uqa_core::ordering::sort_by_with_control(rows, &mut || Ok(()), |left, right, _| {
        compare(left, right)
    })
}

pub(super) fn minimum_by<T>(
    items: impl Iterator<Item = T>,
    compare: impl Fn(&T, &T) -> Result<Ordering, SQLError>,
) -> Result<Option<T>, SQLError> {
    let mut minimum = None;
    for item in items {
        if match &minimum {
            Some(current) => compare(&item, current)?.is_lt(),
            None => true,
        } {
            minimum = Some(item);
        }
    }
    Ok(minimum)
}
