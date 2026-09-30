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

/// One evaluated aggregate `ORDER BY` key with its direction and NULL placement.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AggregateSortKey {
    pub(super) value: Value,
    pub(super) descending: bool,
    pub(super) nulls_first: bool,
}

impl AggregateSortKey {
    /// A key of an aggregate's `ORDER BY` item; NULLs come first by default only when descending.
    pub(super) fn ordered(value: Value, order: &uqa_sql::ScalarOrder) -> Self {
        Self {
            value,
            descending: order.descending,
            nulls_first: order.nulls.map_or(order.descending, |nulls| {
                nulls == uqa_sql::ast::NullsOrder::First
            }),
        }
    }

    /// The default ascending key, with NULLs last.
    pub(super) fn ascending(value: Value) -> Self {
        Self::directed(value, false)
    }

    /// A key with `PostgreSQL`'s default NULL placement for its direction.
    pub(super) fn directed(value: Value, descending: bool) -> Self {
        Self {
            value,
            descending,
            nulls_first: descending,
        }
    }
}

pub(super) fn compare_sort_keys(
    left: &[AggregateSortKey],
    right: &[AggregateSortKey],
) -> Result<Ordering, SQLError> {
    for (left, right) in left.iter().zip(right) {
        let ordering = match (
            matches!(left.value, Value::Null),
            matches!(right.value, Value::Null),
        ) {
            (true, true) => Ordering::Equal,
            (true, false) if left.nulls_first => Ordering::Less,
            (true, false) => Ordering::Greater,
            (false, true) if left.nulls_first => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                let ordering = uqa_sql::expr::compare_typed_values_with_control(
                    &left.value,
                    &right.value,
                    &ProductionControl::uncontrolled(),
                )?;
                if left.descending {
                    ordering.reverse()
                } else {
                    ordering
                }
            }
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
