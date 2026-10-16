//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::spill::format::{decode_physical_row_record, encode_physical_row_record};

#[test]
fn bit_string_lengths_and_nested_values_survive_actual_binary_spill() {
    let values = [
        (vec![], 0),
        (vec![0x80], 1),
        (vec![0x80], 8),
        (vec![0xaa, 0x80], 9),
    ]
    .into_iter()
    .map(|(bytes, length)| {
        Value::BitString(uqa_core::BitStringValue::try_from_packed(bytes, length).unwrap())
    })
    .collect::<Vec<_>>();
    let nested = Value::Record(vec![("bits".into(), Value::List(values.clone()))].into());
    let mut row_values = values;
    row_values.push(nested);
    let width = row_values.len();
    let row = PhysicalRow::from_values(row_values);
    let encoded = encode_physical_row_record(&row, width).unwrap();
    for length in 0..encoded.len() {
        assert!(decode_physical_row_record(&encoded[..length], width).is_err());
    }
    let decoded = decode_physical_row_record(&encoded, width).unwrap();
    for slot in 0..width {
        assert!(decoded
            .value(slot)
            .unwrap()
            .has_same_representation(row.value(slot).unwrap()));
    }
    let mut spill = SpillBuffer::new(0);
    spill
        .push(Batch::from_physical_rows(
            RowSchema::new((0..width).map(|i| format!("b{i}")).collect()),
            vec![row.clone()],
        ))
        .unwrap();
    let restored = spill.drain_all().unwrap();
    for slot in 0..width {
        assert!(restored[0].rows[0]
            .value(slot)
            .unwrap()
            .has_same_representation(row.value(slot).unwrap()));
    }
    let one = PhysicalRow::from_values(vec![Value::BitString(
        uqa_core::BitStringValue::try_from_packed(vec![0x80], 1).unwrap(),
    )]);
    let mut malformed = encode_physical_row_record(&one, 1).unwrap();
    *malformed.last_mut().unwrap() = 0x81;
    assert!(decode_physical_row_record(&malformed, 1)
        .unwrap_err()
        .to_string()
        .contains("invalid packed bit padding"));
}

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
    let value = Value::Record(
        vec![
            ("a".into(), Value::Datum(datum.clone())),
            ("b".into(), Value::Datum(datum.field(25, u32::MAX))),
        ]
        .into(),
    );
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

#[test]
fn named_record_identity_survives_spill_and_rejects_truncated_metadata() {
    let value = Value::Record(uqa_core::RecordValue::from_parts(
        vec![("n".into(), Value::Float(0.1))],
        Some(20_001),
    ));
    let row = PhysicalRow::from_values(vec![value.clone()]);
    let encoded = encode_physical_row_record(&row, 1).unwrap();
    for length in 0..encoded.len() {
        assert!(decode_physical_row_record(&encoded[..length], 1).is_err());
    }
    let decoded = decode_physical_row_record(&encoded, 1).unwrap();
    assert!(decoded.value(0).unwrap().has_same_representation(&value));
    let mut buffer = SpillBuffer::new(0);
    buffer
        .push(Batch::from_physical_rows(
            RowSchema::new(vec!["v".into()]),
            vec![row],
        ))
        .unwrap();
    let restored = buffer.drain_all().unwrap();
    assert!(restored[0].rows[0]
        .value(0)
        .unwrap()
        .has_same_representation(&value));
}
