//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::spill::format::{decode_physical_row_record, encode_physical_row_record};
use uqa_core::{LegacyVectorKind, LegacyVectorValue};

fn vector(kind: LegacyVectorKind, elements: &[i64]) -> Value {
    Value::LegacyVector(
        LegacyVectorValue::try_new(kind, elements.iter().copied().map(Value::Int).collect())
            .unwrap(),
    )
}

#[test]
fn legacy_vectors_round_trip_in_batches_and_indexed_records() {
    for kind in [LegacyVectorKind::SmallInteger, LegacyVectorKind::Oid] {
        let bounds = match kind {
            LegacyVectorKind::SmallInteger => [-32768, 32767],
            LegacyVectorKind::Oid => [0, 4_294_967_295],
        };
        let empty = vector(kind, &[]);
        let nonempty = vector(kind, &bounds);
        let nested = Value::Array(
            ArrayValue::with_lower_bounds(vec![empty.clone(), nonempty.clone()], vec![-3]).unwrap(),
        );
        let shifted = Value::LegacyVector(
            LegacyVectorValue::try_from_array(
                kind,
                ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![1]).unwrap(),
            )
            .unwrap(),
        );
        let dimensionless = Value::LegacyVector(
            LegacyVectorValue::try_from_array(kind, ArrayValue::try_new(vec![]).unwrap()).unwrap(),
        );
        let row = PhysicalRow::from_values(vec![empty, nonempty, nested, shifted, dimensionless]);
        let encoded = encode_physical_row_record(&row, 5).unwrap();
        let decoded = decode_physical_row_record(&encoded, 5).unwrap();
        for index in 0..5 {
            assert_eq!(decoded.value(index), row.value(index));
            assert_eq!(
                decoded.value(index).unwrap().array_view(),
                row.value(index).unwrap().array_view()
            );
        }
        let mut buffer = SpillBuffer::new(0);
        buffer
            .push(Batch::from_physical_rows(
                RowSchema::new(vec![
                    "empty".into(),
                    "nonempty".into(),
                    "nested".into(),
                    "shifted".into(),
                    "dimensionless".into(),
                ]),
                vec![row.clone()],
            ))
            .unwrap();
        let restored = buffer.drain_all().unwrap();
        for index in 0..5 {
            assert_eq!(restored[0].rows[0].value(index), row.value(index));
            assert_eq!(
                restored[0].rows[0].value(index).unwrap().array_view(),
                row.value(index).unwrap().array_view()
            );
        }
    }
}

#[test]
fn legacy_vector_spill_rejects_corrupt_kind_lengths_and_element_widths() {
    let row = PhysicalRow::from_values(vec![vector(LegacyVectorKind::SmallInteger, &[1])]);
    let encoded = encode_physical_row_record(&row, 1).unwrap();
    for length in 0..encoded.len() {
        assert!(decode_physical_row_record(&encoded[..length], 1).is_err());
    }
    let mut unknown_kind = encoded.clone();
    unknown_kind[9] = 2;
    assert!(decode_physical_row_record(&unknown_kind, 1).is_err());
    let mut huge_count = encoded.clone();
    huge_count[22..30].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(decode_physical_row_record(&huge_count, 1).is_err());
    for (kind, value) in [(0, 32768_i64), (0, -32769), (1, -1), (1, 4_294_967_296)] {
        let mut record = encoded.clone();
        record[9] = kind;
        record[31..39].copy_from_slice(&value.to_le_bytes());
        assert!(decode_physical_row_record(&record, 1).is_err());
    }
    let mut null_element = encoded[..31].to_vec();
    null_element[30] = 0;
    assert!(decode_physical_row_record(&null_element, 1).is_err());
    let mut float_element = encoded;
    float_element[30] = 3;
    float_element[31..39].copy_from_slice(&1.0_f64.to_bits().to_le_bytes());
    assert!(decode_physical_row_record(&float_element, 1).is_err());
}
