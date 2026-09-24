//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded, ordered DISTINCT input and aggregate value comparisons.

use super::{AggregateValueBuffer, SQLError, Value};
use uqa_core::memory::ProductionControl;

/// Aggregate DISTINCT uses SQL ordering before eliminating adjacent equal inputs.
/// Reusing sorted runs keeps both comparisons and spill behavior fallible.
pub struct DistinctTracker {
    pub(super) values: AggregateValueBuffer,
}

impl Default for DistinctTracker {
    fn default() -> Self {
        Self::new(32 * 1024 * 1024)
    }
}

impl DistinctTracker {
    pub(super) fn new(budget_bytes: usize) -> Self {
        Self {
            values: AggregateValueBuffer::new(budget_bytes),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.values.next_sequence == 0
    }

    pub(super) fn insert(
        &mut self,
        value: &Value,
        mut sort_keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        // Explicit ORDER BY keys come first; the complete argument tuple resolves their ties.
        sort_keys.push((value.clone(), false));
        self.values.push(value.clone(), sort_keys)
    }

    pub(super) fn for_each(
        &self,
        mut observe: impl FnMut(&Value) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let mut previous: Option<Value> = None;
        self.values.for_each_ordered(|record| {
            if let Some(previous) = &previous {
                if uqa_sql::expr::compare_typed_values_with_control(
                    previous,
                    &record.value,
                    &ProductionControl::uncontrolled(),
                )?
                .is_eq()
                {
                    return Ok(());
                }
            }
            observe(&record.value)?;
            previous = Some(record.value);
            Ok(())
        })
    }
}

pub fn value_as_f64(v: &Value) -> Result<f64, SQLError> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::Decimal(d) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch(format!("expected number that fits float, got {v:?}"))
        }),
        other => Err(SQLError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

pub fn value_lt(a: &Value, b: &Value) -> Result<bool, SQLError> {
    Ok(match (a, b) {
        (
            Value::Int(_) | Value::Float(_) | Value::Decimal(_),
            Value::Int(_) | Value::Float(_) | Value::Decimal(_),
        ) => a < b,
        (Value::Str(x), Value::Str(y)) => x < y,
        (Value::FixedChar(x), Value::FixedChar(y)) => x.trim_end() < y.trim_end(),
        (Value::Bytes(x), Value::Bytes(y)) => x < y,
        (Value::Temporal(x), Value::Temporal(y)) => x < y,
        // PostgreSQL MIN/MAX bind array_smaller/array_larger for catalog vectors; OID-vector scalar operators use a different order.
        (Value::LegacyVector(left), Value::LegacyVector(right)) => {
            left.compare_as_array(right).is_lt()
        }
        (Value::Array(_), Value::Array(_))
        | (Value::List(_), Value::List(_))
        | (Value::Row(_), Value::Row(_))
        | (Value::Record(_), Value::Record(_)) => super::compare_extrema(a, b)?.is_lt(),
        (Value::Map(x), Value::Map(y)) => x < y,
        _ => false,
    })
}

pub fn value_gt(a: &Value, b: &Value) -> Result<bool, SQLError> {
    value_lt(b, a)
}
