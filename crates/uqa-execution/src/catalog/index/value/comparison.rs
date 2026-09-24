//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL comparisons for keys whose operators cannot be represented by an infallible B-tree comparator.

use uqa_core::{memory::ProductionControl, Predicate, Value};
use uqa_sql::{expr::value_comparison_can_fail, SQLError};

pub(super) fn needs_sql_comparison(predicate: &Predicate, stored_can_fail: bool) -> bool {
    match predicate {
        Predicate::IsNull | Predicate::IsNotNull => false,
        Predicate::Equals(value)
        | Predicate::NotEquals(value)
        | Predicate::GreaterThan(value)
        | Predicate::GreaterThanOrEqual(value)
        | Predicate::LessThan(value)
        | Predicate::LessThanOrEqual(value) => stored_can_fail || value_comparison_can_fail(value),
        Predicate::Between { low, high } => {
            stored_can_fail || value_comparison_can_fail(low) || value_comparison_can_fail(high)
        }
        Predicate::InSet(values) => {
            !values.is_empty() && (stored_can_fail || values.iter().any(value_comparison_can_fail))
        }
    }
}

pub(super) fn matches(value: &Value, predicate: &Predicate) -> Result<bool, SQLError> {
    let compare = |other: &Value| {
        uqa_sql::expr::compare_typed_values_with_control(
            value,
            other,
            &ProductionControl::uncontrolled(),
        )
    };
    if matches!(value, Value::Null) {
        return Ok(matches!(predicate, Predicate::IsNull));
    }
    Ok(match predicate {
        Predicate::Equals(other) => !matches!(other, Value::Null) && compare(other)?.is_eq(),
        Predicate::NotEquals(other) => !matches!(other, Value::Null) && !compare(other)?.is_eq(),
        Predicate::GreaterThan(other) => !matches!(other, Value::Null) && compare(other)?.is_gt(),
        Predicate::GreaterThanOrEqual(other) => {
            !matches!(other, Value::Null) && !compare(other)?.is_lt()
        }
        Predicate::LessThan(other) => !matches!(other, Value::Null) && compare(other)?.is_lt(),
        Predicate::LessThanOrEqual(other) => {
            !matches!(other, Value::Null) && !compare(other)?.is_gt()
        }
        Predicate::Between { low, high } => {
            !matches!(low, Value::Null)
                && !compare(low)?.is_lt()
                && !matches!(high, Value::Null)
                && !compare(high)?.is_gt()
        }
        Predicate::InSet(values) => {
            for other in values {
                if !matches!(other, Value::Null) && compare(other)?.is_eq() {
                    return Ok(true);
                }
            }
            false
        }
        Predicate::IsNull => false,
        Predicate::IsNotNull => true,
    })
}
