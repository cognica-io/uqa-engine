//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::spill::format::{decode_physical_row_record, encode_physical_row_record};

#[test]
fn retained_array_element_identity_survives_binary_spill() {
    let array = uqa_core::ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![-2])
        .unwrap()
        .with_element_type_oid(Some(23));
    let value = Value::Array(array);
    let row = PhysicalRow::from_values(vec![value.clone()]);
    let encoded = encode_physical_row_record(&row, 1).unwrap();
    for length in 0..encoded.len() {
        assert!(decode_physical_row_record(&encoded[..length], 1).is_err());
    }
    let decoded = decode_physical_row_record(&encoded, 1).unwrap();
    assert!(decoded.value(0).unwrap().has_same_representation(&value));
}

#[test]
fn retained_datums_spill_without_reading_corrupt_physical_fields() {
    let datum = uqa_core::DatumValue::new(1700, 0, vec![2, 0, 0, 0, 3, b'x']);
    let value = Value::Record(vec![
        ("a".into(), Value::Datum(datum.clone())),
        ("b".into(), Value::Datum(datum.field(25, u32::MAX))),
    ]);
    let row = PhysicalRow::from_values(vec![value]);
    let encoded = encode_physical_row_record(&row, 1).unwrap();
    for length in 0..encoded.len() {
        assert!(decode_physical_row_record(&encoded[..length], 1).is_err());
    }
    let decoded = decode_physical_row_record(&encoded, 1).unwrap();
    assert!(decoded
        .value(0)
        .unwrap()
        .has_same_representation(row.value(0).unwrap()));
    let mut buffer = SpillBuffer::new(0);
    buffer
        .push(Batch::from_physical_rows(
            RowSchema::new(vec!["v".into()]),
            vec![row.clone()],
        ))
        .unwrap();
    let restored = buffer.drain_all().unwrap();
    let restored = restored[0].rows[0].value(0).unwrap();
    assert!(restored.has_same_representation(row.value(0).unwrap()));
    let error = uqa_sql::expr::value_to_string(restored).unwrap_err();
    assert_eq!(error.to_string(), "compressed pglz data is corrupt");
}
