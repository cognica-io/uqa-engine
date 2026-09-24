//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained value identity preserves representation details that SQL equality ignores.

use super::{ArrayValue, TemporalValue, Value};

impl Value {
    /// Compare retained variants and payloads without SQL coercion or normalization. Index-entry retention must distinguish signed zero, numeric scale, interval fields and nested array metadata even when the values compare equal.
    pub fn has_same_representation(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) | (Self::Void, Self::Void) => true,
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::Int(left), Self::Int(right)) => left == right,
            (Self::Float(left), Self::Float(right)) => left.to_bits() == right.to_bits(),
            (Self::Decimal(left), Self::Decimal(right)) => left.has_same_representation(right),
            (Self::Str(left), Self::Str(right))
            | (Self::FixedChar(left), Self::FixedChar(right))
            | (Self::Json(left), Self::Json(right))
            | (Self::JsonB(left), Self::JsonB(right)) => left == right,
            (Self::Bytes(left), Self::Bytes(right)) => left == right,
            (Self::Temporal(left), Self::Temporal(right)) => same_temporal(left, right),
            (Self::Array(left), Self::Array(right)) => same_array(left, right),
            (Self::LegacyVector(left), Self::LegacyVector(right)) => {
                left.kind() == right.kind() && same_array(left.as_array(), right.as_array())
            }
            (Self::List(left), Self::List(right)) | (Self::Row(left), Self::Row(right)) => {
                same_elements(left, right)
            }
            (Self::Record(left), Self::Record(right)) => {
                left.len() == right.len()
                    && left.iter().zip(right).all(|((a, left), (b, right))| {
                        a == b && left.has_same_representation(right)
                    })
            }
            (Self::Map(left), Self::Map(right)) => {
                left.len() == right.len()
                    && left.iter().zip(right).all(|((a, left), (b, right))| {
                        a == b && left.has_same_representation(right)
                    })
            }
            _ => false,
        }
    }
}

fn same_elements(left: &[Value], right: &[Value]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.has_same_representation(right))
}

fn same_array(left: &ArrayValue, right: &ArrayValue) -> bool {
    left.dimensions() == right.dimensions()
        && left.lower_bounds() == right.lower_bounds()
        && same_elements(left.elements(), right.elements())
}

fn same_temporal(left: &TemporalValue, right: &TemporalValue) -> bool {
    use TemporalValue::{Date, Interval, Time, TimeTz, Timestamp, TimestampTz};
    match (left, right) {
        (Date { days: left }, Date { days: right }) => left == right,
        (Time { micros: left }, Time { micros: right })
        | (Timestamp { micros: left }, Timestamp { micros: right })
        | (TimestampTz { micros: left }, TimestampTz { micros: right }) => left == right,
        (
            TimeTz {
                micros: a,
                offset_minutes: a_offset,
            },
            TimeTz {
                micros: b,
                offset_minutes: b_offset,
            },
        ) => a == b && a_offset == b_offset,
        (
            Interval {
                months: a_months,
                days: a_days,
                micros: a,
            },
            Interval {
                months: b_months,
                days: b_days,
                micros: b,
            },
        ) => a_months == b_months && a_days == b_days && a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DecimalValue, LegacyVectorKind, LegacyVectorValue};

    #[test]
    fn legacy_vector_index_retention_distinguishes_equal_storage_representations() {
        let vector = |lower| {
            Value::LegacyVector(
                LegacyVectorValue::try_from_array(
                    LegacyVectorKind::Oid,
                    ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![lower]).unwrap(),
                )
                .unwrap(),
            )
        };
        let pairs = [
            (Value::Float(0.0), Value::Float(-0.0)),
            (
                Value::Float(f64::NAN),
                Value::Float(f64::from_bits(f64::NAN.to_bits() ^ 1)),
            ),
            (
                Value::Decimal(DecimalValue::parse("1.0").unwrap()),
                Value::Decimal(DecimalValue::parse("1.00").unwrap()),
            ),
            (
                Value::Temporal(TemporalValue::Interval {
                    months: 1,
                    days: 0,
                    micros: 0,
                }),
                Value::Temporal(TemporalValue::Interval {
                    months: 0,
                    days: 30,
                    micros: 0,
                }),
            ),
            (Value::FixedChar("a".into()), Value::FixedChar("a ".into())),
            (Value::JsonB("1.0".into()), Value::JsonB("1.00".into())),
            (Value::Int(1), Value::Float(1.0)),
            (vector(0), vector(1)),
        ];
        for (left, right) in pairs {
            assert_eq!(left, right);
            assert!(left.has_same_representation(&left.clone()));
            assert!(right.has_same_representation(&right.clone()));
            assert!(!left.has_same_representation(&right));
            assert!(!right.has_same_representation(&left));
            let wrappers: [fn(Value) -> Value; 4] = [
                |value| Value::Array(ArrayValue::try_new(vec![value]).unwrap()),
                |value| Value::Row(vec![value]),
                |value| Value::Record(vec![("key".into(), value)]),
                |value| Value::Map(std::collections::BTreeMap::from([("key".into(), value)])),
            ];
            for wrap in wrappers {
                let wrapped = wrap(left.clone());
                assert!(wrapped.has_same_representation(&wrapped.clone()));
                assert!(!wrapped.has_same_representation(&wrap(right.clone())));
            }
        }
    }
}
