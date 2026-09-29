//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::spill::format::{decode_physical_row_record, encode_physical_row_record};
use uqa_core::{EnumLabelKey, EnumValue};

fn label(type_oid: u32, key: &[u8]) -> Value {
    Value::Enum(EnumValue::new(
        type_oid,
        EnumLabelKey::from_bytes(key.to_vec()).unwrap(),
    ))
}

#[test]
fn enum_values_round_trip_in_batches_and_indexed_records() {
    let nested = Value::Array(
        ArrayValue::with_lower_bounds(vec![label(9, &[1]), Value::Null], vec![0]).unwrap(),
    );
    let row = PhysicalRow::from_values(vec![
        label(16_390, &[64]),
        label(u32::MAX, &[0, 255, 7]),
        nested,
    ]);
    let encoded = encode_physical_row_record(&row, 3).unwrap();
    let decoded = decode_physical_row_record(&encoded, 3).unwrap();
    for index in 0..3 {
        assert!(decoded
            .value(index)
            .unwrap()
            .has_same_representation(row.value(index).unwrap()));
    }
    let mut buffer = SpillBuffer::new(0);
    buffer
        .push(Batch::from_physical_rows(
            RowSchema::new(vec!["first".into(), "last".into(), "nested".into()]),
            vec![row.clone()],
        ))
        .unwrap();
    let restored = buffer.drain_all().unwrap();
    for index in 0..3 {
        assert!(restored[0].rows[0]
            .value(index)
            .unwrap()
            .has_same_representation(row.value(index).unwrap()));
    }
}

#[test]
fn enum_spill_rejects_truncated_records_and_invalid_keys() {
    let row = PhysicalRow::from_values(vec![label(7, &[5, 9])]);
    let encoded = encode_physical_row_record(&row, 1).unwrap();
    for length in 0..encoded.len() {
        assert!(decode_physical_row_record(&encoded[..length], 1).is_err());
    }
    // The key's final byte is the record's last byte; zeroing it violates the key invariant.
    let mut corrupt = encoded.clone();
    *corrupt.last_mut().unwrap() = 0;
    let error = decode_physical_row_record(&corrupt, 1)
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid enum label key"), "{error}");
}
