//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reservation aliases keep mixed-version processes on the same conflict lock.

use super::{canonical_row_key, encode_len, encode_value, ExecResult, KeyOutput, Value};

pub(crate) fn canonical_row_lock_keys(values: &[Value]) -> ExecResult<[Vec<u8>; 2]> {
    let mut legacy = LegacyNumericReservation(Vec::new());
    encode_len(values.len(), &mut legacy)?;
    for value in values {
        encode_value(value, &mut legacy)?;
    }
    Ok([canonical_row_key(values)?, legacy.0])
}

struct LegacyNumericReservation(Vec<u8>);

impl KeyOutput for LegacyNumericReservation {
    fn push_byte(&mut self, value: u8) -> ExecResult<()> {
        self.0.push(value);
        Ok(())
    }

    fn extend_bytes(&mut self, values: &[u8]) -> ExecResult<()> {
        self.0.extend_from_slice(values);
        Ok(())
    }

    fn legacy_numeric_reservation(&self) -> bool {
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
            let values = [Value::Row(vec![Value::Float(value)])];
            let [exact, legacy] = canonical_row_lock_keys(&values).unwrap();
            let predecessor = canonical_row_key(&[Value::Row(vec![Value::Decimal(
                DecimalValue::parse(displayed).unwrap(),
            )])])
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
            let [exact, legacy] = canonical_row_lock_keys(&[value]).unwrap();
            assert_eq!(exact, legacy);
        }
    }
}
