//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL comparisons for keys whose operators cannot be represented by an infallible B-tree comparator.

use uqa_core::{memory::ProductionControl, Predicate, Value};
use uqa_sql::{expr::value_comparison_can_fail, SQLError};

/// A fallible key also needs evaluation for raw field probes. These requirements
/// only become stricter as keys are added; rebuilding or clearing resets them.
#[derive(Clone, Copy, Default)]
pub(super) enum StoredComparison {
    #[default]
    Ordered,
    TypedField,
    Fallible,
}

impl StoredComparison {
    pub(super) fn include(&mut self, value: &Value) {
        if value_comparison_can_fail(value) {
            *self = Self::Fallible;
        } else if !self.can_fail() && field_needs_typed_comparison(value) {
            *self = Self::TypedField;
        }
    }

    pub(super) fn can_fail(self) -> bool {
        matches!(self, Self::Fallible)
    }

    pub(super) fn field_needs_evaluation(self) -> bool {
        !matches!(self, Self::Ordered)
    }
}

/// Raw field probes are not SQL-bound: enum operands can have different types,
/// and record/anonymous-row equality crosses distinct storage key variants.
pub(super) fn field_needs_typed_comparison(value: &Value) -> bool {
    match value {
        Value::Enum(_) | Value::Record(_) => true,
        Value::Array(array) => array.elements().iter().any(field_needs_typed_comparison),
        Value::Row(values) => values.iter().any(field_needs_typed_comparison),
        Value::List(values) => values.iter().any(field_needs_typed_comparison),
        _ => false,
    }
}

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
