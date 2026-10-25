//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, ArrayValue, CancellationToken, DatumValue, TemporalValue};

#[test]
fn physical_leaf_encoding_borrows_payloads_and_uses_inline_fixed_width_scratch() {
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for (value, ty, expected) in [
        (
            Value::Int(0x1234),
            ColumnType::SmallInteger,
            vec![0x34, 0x12],
        ),
        (
            Value::Int(0x1234_5678),
            ColumnType::Integer,
            vec![0x78, 0x56, 0x34, 0x12],
        ),
        (
            Value::Temporal(TemporalValue::Date { days: 10957 }),
            ColumnType::Date,
            vec![0; 4],
        ),
        (
            Value::Temporal(TemporalValue::TimeTz {
                micros: 1,
                offset_seconds: 3600,
            }),
            ColumnType::TimeTz,
            [
                1_i64.to_le_bytes().as_slice(),
                (-3600_i32).to_le_bytes().as_slice(),
            ]
            .concat(),
        ),
    ] {
        let output = encode(&value, &ty, None, &control).unwrap();
        assert_eq!(&*output, expected);
        assert!(matches!(output, EncodedDatum::Inline { .. }));
    }
    let text = Value::Str("x".repeat(8192));
    let Value::Str(source) = &text else {
        unreachable!()
    };
    let output = encode(&text, &ColumnType::Text, None, &control).unwrap();
    assert_eq!(output.as_ptr(), source.as_ptr());
    let retained = Value::Datum(DatumValue::new(25, 1, vec![0xff, 7, b'a', b'b']));
    assert_eq!(
        &*encode(&retained, &ColumnType::Text, None, &control).unwrap(),
        b"ab"
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn physical_array_encoding_bounds_workspace_and_releases_nested_failures() {
    let token = CancellationToken::new();
    for count in [32, 256, 2048] {
        let value =
            Value::Array(ArrayValue::try_new((0..count).map(Value::Int).collect()).unwrap());
        let memory = MemoryBudget::new(count as usize * 32 + 512);
        let control = ProductionControl::new(&memory, &token, &token);
        let output = encode(
            &value,
            &ColumnType::Array(Box::new(ColumnType::Integer)),
            None,
            &control,
        )
        .unwrap();
        assert_eq!(output.len(), 20 + count as usize * 4);
        assert!(memory.peak() <= count as usize * 32 + 512);
        if let EncodedDatum::Owned(bytes) = &output {
            assert_eq!(memory.used(), bytes.reserved_bytes());
        } else {
            panic!("owned array")
        }
        drop(output);
        assert_eq!(memory.used(), 0);
    }
    let malformed = Value::Array(
        ArrayValue::try_new(vec![
            Value::JsonB("{\"a\":1}".into()),
            Value::Datum(DatumValue::new(3802, u32::MAX, vec![])),
        ])
        .unwrap(),
    );
    let memory = MemoryBudget::new(16384);
    let control = ProductionControl::new(&memory, &token, &token);
    let error = encode(
        &malformed,
        &ColumnType::Array(Box::new(ColumnType::JsonB)),
        None,
        &control,
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("XX001"));
    assert_eq!(memory.used(), 0);
}

#[test]
fn physical_legacy_vectors_share_the_array_codec_and_caller_allowance() {
    use uqa_core::{LegacyVectorKind, LegacyVectorValue};

    let token = CancellationToken::new();
    for (kind, ty, element) in [
        (
            LegacyVectorKind::SmallInteger,
            ColumnType::Int2Vector,
            ColumnType::SmallInteger,
        ),
        (
            LegacyVectorKind::Oid,
            ColumnType::OidVector,
            ColumnType::Oid,
        ),
    ] {
        for count in [0, 1, 256] {
            let vector =
                LegacyVectorValue::try_new(kind, (0..count).map(Value::Int).collect()).unwrap();
            let memory = MemoryBudget::new(8192);
            let control = ProductionControl::new(&memory, &token, &token);
            let expected = composites::retained::array_payload_with_control(
                vector.as_array(),
                &element,
                None,
                &control,
            )
            .unwrap();
            let value = Value::LegacyVector(vector);
            let encoded = encode(&value, &ty, None, &control).unwrap();
            assert_eq!(&*encoded, &*expected);
            assert_eq!(
                encoded.len(),
                20 + count as usize * pg_type_len(&element) as usize
            );
            let EncodedDatum::Owned(bytes) = encoded else {
                panic!("owned vector image");
            };
            assert_eq!(
                memory.used(),
                expected.reserved_bytes() + bytes.reserved_bytes()
            );
            drop((expected, bytes));
            assert_eq!(memory.used(), 0);
            let wrong_type = if ty == ColumnType::Int2Vector {
                ColumnType::OidVector
            } else {
                ColumnType::Int2Vector
            };
            assert_eq!(
                encode(&value, &wrong_type, None, &control)
                    .err()
                    .unwrap()
                    .sqlstate(),
                Some("XX001")
            );
            let exhausted = MemoryBudget::new(0);
            let control = ProductionControl::new(&exhausted, &token, &token);
            assert_eq!(
                encode(&value, &ty, None, &control)
                    .err()
                    .unwrap()
                    .sqlstate(),
                Some("53200")
            );
            assert_eq!(exhausted.used(), 0);
        }
    }
}
