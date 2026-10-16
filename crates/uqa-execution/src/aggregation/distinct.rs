//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded, ordered DISTINCT input and aggregate value comparisons.

use super::{AggregateValueBuffer, SQLError, Value};
use uqa_core::memory::ProductionControl;
use uqa_sql::expr::enums::EnumLabelCatalog;

/// Aggregate DISTINCT uses SQL ordering before eliminating adjacent equal inputs.
/// Reusing sorted runs keeps both comparisons and spill behavior fallible.
pub struct DistinctTracker {
    pub(super) values: AggregateValueBuffer,
    pub(super) argument_count: Option<usize>,
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
            argument_count: None,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.values.next_sequence == 0
    }

    pub(super) fn insert(
        &mut self,
        value: &Value,
        mut sort_keys: Vec<super::ordering::AggregateSortKey>,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        // Explicit ORDER BY keys come first; each selected argument resolves their ties.
        if let Some(count) = self.argument_count {
            for value in arguments(value, count)? {
                sort_keys.push(super::ordering::AggregateSortKey::ascending(value.clone()));
            }
        } else {
            sort_keys.push(super::ordering::AggregateSortKey::ascending(value.clone()));
        }
        self.values.push(value.clone(), sort_keys, enums)
    }

    pub(super) fn for_each(
        &self,
        enums: Option<&dyn EnumLabelCatalog>,
        mut observe: impl FnMut(&Value) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let mut previous: Option<Value> = None;
        self.values.for_each_ordered(enums, |record| {
            if let Some(previous) = &previous {
                if self.equal_inputs(previous, &record.value, enums)? {
                    return Ok(());
                }
            }
            observe(&record.value)?;
            previous = Some(record.value);
            Ok(())
        })
    }

    fn equal_inputs(
        &self,
        left: &Value,
        right: &Value,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<bool, SQLError> {
        let types = self.values.key_types.as_deref().unwrap_or_default();
        let control = ProductionControl::uncontrolled();
        let Some(count) = self.argument_count else {
            return uqa_sql::expr::equal_selected_values_with_catalog(
                left,
                right,
                types.last().and_then(Option::as_ref),
                &control,
                enums,
            );
        };
        let types = types
            .get(types.len().saturating_sub(count)..)
            .filter(|types| types.len() == count)
            .ok_or_else(|| SQLError::Internal("DISTINCT argument types are missing".into()))?;
        for ((left, right), ty) in arguments(left, count)?
            .iter()
            .zip(arguments(right, count)?)
            .zip(types)
        {
            if !uqa_sql::expr::equal_selected_values_with_catalog(
                left,
                right,
                ty.as_ref(),
                &control,
                enums,
            )? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn arguments(value: &Value, count: usize) -> Result<&[Value], SQLError> {
    if let Value::List(values) = value {
        if values.len() == count {
            return Ok(values);
        }
    }
    Err(SQLError::Internal(
        "DISTINCT input does not match its argument tuple".into(),
    ))
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
        // MIN and MAX over `anyenum` use `enum_smaller` and `enum_larger`: label keys in declaration order.
        (Value::Enum(x), Value::Enum(y)) => {
            if x.type_oid() != y.type_oid() {
                return Err(SQLError::Internal(format!(
                    "MIN/MAX received enum type OIDs {} and {}",
                    x.type_oid(),
                    y.type_oid()
                )));
            }
            x.key() < y.key()
        }
        _ => false,
    })
}

pub fn value_gt(a: &Value, b: &Value) -> Result<bool, SQLError> {
    value_lt(b, a)
}
