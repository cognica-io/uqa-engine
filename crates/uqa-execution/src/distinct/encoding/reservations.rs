//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reservation aliases keep mixed-version processes on the same conflict lock.

use super::{canonical_row_key, encode_len, encode_value, ExecResult, KeyOutput, Value};
use smallvec::{smallvec, SmallVec};
use uqa_sql::expr::SQLValueCatalog;

#[cfg(test)]
pub(crate) fn canonical_row_lock_keys(values: &[Value]) -> ExecResult<[Vec<u8>; 3]> {
    let [previous, legacy] = legacy_row_lock_keys(values)?;
    Ok([canonical_row_key(values)?, previous, legacy])
}

/// Selected physical values and range fields use their SQL equality class for reservations. Observation does not invoke SQL hash support; predecessor aliases keep their original representation.
pub(crate) fn canonical_row_lock_keys_with_catalog(
    values: &[Value],
    catalog: &dyn SQLValueCatalog,
) -> ExecResult<SmallVec<[Vec<u8>; 4]>> {
    let current = comparison_key(values, Some(catalog))?;
    let [previous, legacy] = legacy_row_lock_keys(values)?;
    let mut keys = smallvec![current.bytes, previous, legacy];
    if current.normalized {
        keys.push(canonical_row_key(values)?);
    }
    Ok(keys)
}

/// Exact equality for already admitted constraint values, without invoking SQL hash support or producing compatibility lock aliases.
pub(crate) fn canonical_constraint_key(
    values: &[Value],
    catalog: Option<&dyn SQLValueCatalog>,
) -> ExecResult<Vec<u8>> {
    Ok(comparison_key(values, catalog)?.bytes)
}

fn comparison_key<'a>(
    values: &[Value],
    catalog: Option<&'a dyn SQLValueCatalog>,
) -> ExecResult<DeclaredReservation<'a>> {
    let mut current = DeclaredReservation {
        bytes: Vec::new(),
        catalog,
        normalized: false,
    };
    encode_len(values.len(), &mut current)?;
    for value in values {
        encode_value(value, &mut current)?;
    }
    Ok(current)
}

fn legacy_row_lock_keys(values: &[Value]) -> ExecResult<[Vec<u8>; 2]> {
    let mut previous = ReservationAlias {
        bytes: Vec::new(),
        legacy_numeric: false,
    };
    let mut legacy = ReservationAlias {
        bytes: Vec::new(),
        legacy_numeric: true,
    };
    for output in [&mut previous, &mut legacy] {
        encode_len(values.len(), output)?;
        for value in values {
            encode_value(value, output)?;
        }
    }
    Ok([previous.bytes, legacy.bytes])
}

struct DeclaredReservation<'a> {
    bytes: Vec<u8>,
    catalog: Option<&'a dyn SQLValueCatalog>,
    normalized: bool,
}

impl KeyOutput for DeclaredReservation<'_> {
    fn enum_catalog(&self) -> Option<&dyn SQLValueCatalog> {
        self.catalog
    }
    fn uses_declared_field_types(&self) -> bool {
        true
    }
    fn encoded_range_field(&mut self) {
        self.normalized = true;
    }
    fn decoded_physical_value(&mut self) {
        self.normalized = true;
    }
    fn push_byte(&mut self, value: u8) -> ExecResult<()> {
        self.bytes.push(value);
        Ok(())
    }
    fn extend_bytes(&mut self, values: &[u8]) -> ExecResult<()> {
        self.bytes.extend_from_slice(values);
        Ok(())
    }
}

struct ReservationAlias {
    bytes: Vec<u8>,
    legacy_numeric: bool,
}

impl KeyOutput for ReservationAlias {
    fn push_byte(&mut self, value: u8) -> ExecResult<()> {
        self.bytes.push(value);
        Ok(())
    }

    fn extend_bytes(&mut self, values: &[u8]) -> ExecResult<()> {
        self.bytes.extend_from_slice(values);
        Ok(())
    }

    fn legacy_numeric_reservation(&self) -> bool {
        self.legacy_numeric
    }

    fn legacy_temporal_reservation(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::DecimalValue;

    #[test]
    fn numeric_lock_alias_retains_predecessor_bytes_without_weakening_equality() {
        for (value, displayed) in [
            (0.1, "0.1"),
            (9_223_372_036_854_774_784_i64 as f64, "9223372036854775000"),
        ] {
            let values = [Value::Row(vec![Value::Float(value)].into())];
            let [exact, previous, legacy] = canonical_row_lock_keys(&values).unwrap();
            assert_eq!(exact, previous);
            let predecessor = canonical_row_key(&[Value::Row(
                vec![Value::Decimal(DecimalValue::parse(displayed).unwrap())].into(),
            )])
            .unwrap();
            assert_eq!(legacy, predecessor);
            assert_eq!(exact, canonical_row_key(&values).unwrap());
            assert_ne!(exact, legacy);
        }
        for value in [
            Value::Int(1),
            Value::Float(-0.0),
            Value::Float(f64::NAN),
            Value::Float(f64::INFINITY),
        ] {
            let [exact, previous, legacy] = canonical_row_lock_keys(&[value]).unwrap();
            assert_eq!(exact, previous);
            assert_eq!(exact, legacy);
        }
    }

    #[test]
    fn composite_lock_aliases_retain_both_numeric_and_temporal_predecessors() {
        let row = [Value::Row(
            vec![
                Value::Float(0.1),
                Value::Temporal(uqa_core::TemporalValue::TimeTz {
                    micros: 46_800_000_000,
                    offset_minutes: 60,
                }),
            ]
            .into(),
        )];
        let [current, previous, legacy] = canonical_row_lock_keys(&row).unwrap();
        let predecessor = |decimal: &[u8]| {
            // Frozen predecessor layout: one row, two fields, finite numeric text and day-wrapped TIMETZ.
            let mut bytes = vec![0, 0, 0, 0, 0, 0, 0, 1, 10, 0, 0, 0, 0, 0, 0, 0, 2, 1, 0];
            bytes.extend_from_slice(&(decimal.len() as u64).to_be_bytes());
            bytes.extend_from_slice(decimal);
            bytes.extend_from_slice(&[4, 2]);
            bytes.extend_from_slice(&43_200_000_000_i128.to_be_bytes());
            bytes
        };
        assert_eq!(
            previous,
            predecessor(b"0.1000000000000000055511151231257827021181583404541015625")
        );
        assert_eq!(legacy, predecessor(b"0.1"));
        assert_eq!(current, canonical_row_key(&row).unwrap());
        assert_ne!(current, previous);
        assert_ne!(previous, legacy);
        let midnight = [Value::Temporal(uqa_core::TemporalValue::Time { micros: 0 })];
        let endpoint = [Value::Temporal(uqa_core::TemporalValue::Time {
            micros: 86_400_000_000,
        })];
        assert_ne!(
            canonical_row_key(&midnight).unwrap(),
            canonical_row_key(&endpoint).unwrap()
        );
        assert_eq!(
            canonical_row_lock_keys(&midnight).unwrap()[1],
            canonical_row_lock_keys(&endpoint).unwrap()[1]
        );
    }
}
