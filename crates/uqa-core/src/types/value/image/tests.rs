//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ArrayValue, BitStringValue, DatumValue, DecimalValue, EnumLabelKey, EnumValue,
    LegacyVectorKind, LegacyVectorValue, RecordDatumField, RecordFieldType, RecordValue, RowValue,
    TemporalValue,
};
use std::collections::BTreeMap;

mod input;
mod stream;

fn encode(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    let memory = MemoryBudget::new(1 << 20);
    let written = write_value_image(value, &mut bytes, &memory, &CancellationToken::new()).unwrap();
    assert_eq!(written, bytes.len() as u64);
    assert_eq!(memory.used(), 0);
    bytes
}

fn fixtures() -> Vec<Value> {
    let mut values = vec![
        Value::Null,
        Value::Void,
        Value::Bool(true),
        Value::Bool(false),
        Value::Int(i64::MIN),
        Value::Int(i64::MAX),
        Value::Float(-0.0),
        Value::Str("nul\0quote\"한글".into()),
        Value::FixedChar("x  ".into()),
        Value::Bytes(vec![0, 255, 3]),
        Value::Json(" { \"a\":1,\"a\":2 } ".into()),
        Value::JsonB("{\"a\": 2}".into()),
        Value::List(Vec::new()),
        Value::Map(BTreeMap::new()),
        Value::BitString(BitStringValue::try_from_packed(vec![0xa8], 5).unwrap()),
        Value::Datum(
            DatumValue::new(25, u32::MAX, vec![0, 255, 32]).with_record_fields(vec![
                RecordDatumField {
                    name: "duplicate".into(),
                    type_oid: 25,
                    type_modifier: -1,
                    collation_oid: Some(100),
                    length: -1,
                    alignment: 4,
                    dropped: false,
                },
                RecordDatumField {
                    name: "duplicate".into(),
                    type_oid: 0,
                    type_modifier: 0,
                    collation_oid: None,
                    length: i64::MIN,
                    alignment: 0,
                    dropped: true,
                },
            ]),
        ),
        Value::Enum(
            EnumValue::new(91, EnumLabelKey::from_bytes(vec![0, 1, 255]).unwrap())
                .with_label_oid(Some(93)),
        ),
        Value::Row(RowValue::new(vec![Value::Int(1), Value::Null])),
        Value::Row(
            RowValue::typed(
                vec![Value::Int(1), Value::Null],
                vec![
                    RecordFieldType {
                        oid: 23,
                        type_modifier: -1,
                        collation_oid: None,
                    },
                    RecordFieldType {
                        oid: 25,
                        type_modifier: 11,
                        collation_oid: Some(100),
                    },
                ],
            )
            .unwrap(),
        ),
        Value::Record(RecordValue::from_parts(
            vec![
                ("x".into(), Value::Int(1)),
                ("x".into(), Value::Null),
                (String::new(), Value::Void),
            ],
            Some(401),
        )),
    ];
    for text in [
        "0.000",
        "-123456789012345678901234567890.0001",
        "NaN",
        "Infinity",
        "-Infinity",
    ] {
        values.push(Value::Decimal(DecimalValue::parse(text).unwrap()));
    }
    add_temporal_fixtures(&mut values);
    add_array_fixtures(&mut values);
    values
}

fn add_temporal_fixtures(values: &mut Vec<Value>) {
    for value in [
        TemporalValue::Date { days: i32::MIN },
        TemporalValue::Date { days: i32::MAX },
        TemporalValue::Time {
            micros: 86_400_000_000,
        },
        TemporalValue::TimeTz {
            micros: 123,
            offset_seconds: -3599,
        },
        TemporalValue::Timestamp {
            postgres_micros: i64::MIN,
        },
        TemporalValue::TimestampTz {
            postgres_micros: i64::MAX,
        },
        TemporalValue::Interval {
            months: -3,
            days: 11,
            micros: -19,
        },
    ] {
        values.push(Value::Temporal(value));
    }
}

fn add_array_fixtures(values: &mut Vec<Value>) {
    for kind in [LegacyVectorKind::SmallInteger, LegacyVectorKind::Oid] {
        for bound in [-9, 0, 7] {
            let array = ArrayValue::with_lower_bounds(vec![Value::Int(3)], vec![bound])
                .unwrap()
                .with_element_type_oid(Some(999));
            values.push(Value::LegacyVector(
                LegacyVectorValue::try_from_array(kind, array).unwrap(),
            ));
        }
        values.push(Value::LegacyVector(
            LegacyVectorValue::try_new(kind, vec![]).unwrap(),
        ));
    }
    for array in [
        ArrayValue::try_new(vec![]).unwrap(),
        ArrayValue::with_lower_bounds(vec![], vec![0]).unwrap(),
        ArrayValue::with_lower_bounds(vec![Value::Int(5), Value::Null], vec![-7]).unwrap(),
        ArrayValue::with_lower_bounds(
            vec![
                Value::List(vec![Value::Int(1), Value::Null]),
                Value::List(vec![Value::Int(3), Value::Int(4)]),
            ],
            vec![-2, 8],
        )
        .unwrap(),
    ] {
        values.push(Value::Array(array.with_element_type_oid(Some(23))));
    }
}

#[test]
fn value_images_preserve_all_carriers_and_opaque_metadata() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for value in fixtures() {
        let bytes = encode(&value);
        let decoded = decode_value_image(&bytes, &memory, &token).unwrap();
        assert_eq!(&*decoded, &value, "{value:?}");
        // Core equality deliberately ignores some opaque descriptors; verify those too.
        match (&value, &*decoded) {
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.dimensions(), b.dimensions());
                assert_eq!(a.element_type_oid(), b.element_type_oid());
            }
            (Value::LegacyVector(a), Value::LegacyVector(b)) => {
                assert_eq!(a.kind(), b.kind());
                assert_eq!(a.as_array().dimensions(), b.as_array().dimensions());
                assert_eq!(a.as_array().lower_bounds(), b.as_array().lower_bounds());
                assert_eq!(
                    a.as_array().element_type_oid(),
                    b.as_array().element_type_oid()
                );
            }
            (Value::Row(a), Value::Row(b)) => assert_eq!(a.field_types(), b.field_types()),
            (Value::Record(a), Value::Record(b)) => assert_eq!(a.type_oid(), b.type_oid()),
            (Value::Enum(a), Value::Enum(b)) => assert_eq!(a.label_oid(), b.label_oid()),
            _ => {}
        }
        let payload = decoded.retained_payload_bytes(&memory, &token).unwrap();
        assert!(decoded.reserved_bytes() >= payload);
        assert_eq!(bytes, encode(&decoded));
        drop(decoded);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn value_image_literals_preserve_bits_types_and_map_tag_collisions() {
    assert_eq!(encode(&Value::Null), b"UQAVAL01\x00");
    assert_eq!(encode(&Value::Void), b"UQAVAL01\x01");
    assert_eq!(
        encode(&Value::Int(-2)),
        b"UQAVAL01\x03\xfe\xff\xff\xff\xff\xff\xff\xff"
    );
    assert_eq!(
        encode(&Value::Str("a".into())),
        b"UQAVAL01\x05\x01\0\0\0\0\0\0\0a"
    );
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for bits in [
        0,
        1,
        1 << 63,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_1234_5678_9abc,
        0xfff0_0000_0000_0001,
    ] {
        let bytes = encode(&Value::Float(f64::from_bits(bits)));
        assert_eq!(&bytes[8..9], &[4]);
        assert_eq!(&bytes[9..], &bits.to_le_bytes());
        let decoded = decode_value_image(&bytes, &memory, &token).unwrap();
        let Value::Float(value) = *decoded else {
            panic!("float identity")
        };
        assert_eq!(value.to_bits(), bits);
    }
    for fields in [
        BTreeMap::from([("$uqa_type".into(), Value::Str("void".into()))]),
        BTreeMap::from([(
            "$serde_json::private::Number".into(),
            Value::Str("1.0".into()),
        )]),
        BTreeMap::from([
            ("$uqa_type".into(), Value::Str("bytes".into())),
            ("hex".into(), Value::Str("ff".into())),
        ]),
    ] {
        let value = Value::List(vec![Value::Map(fields)]);
        assert_eq!(
            &*decode_value_image(&encode(&value), &memory, &token).unwrap(),
            &value
        );
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn value_image_decoder_rejects_truncation_bad_tags_and_trailing_bytes() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for value in fixtures() {
        let bytes = encode(&value);
        for end in 0..bytes.len() {
            assert!(
                decode_value_image(&bytes[..end], &memory, &token).is_err(),
                "{end} {value:?}"
            );
            assert_eq!(memory.used(), 0);
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            decode_value_image(&trailing, &memory, &token),
            Err(ValueImageError::Invalid("trailing bytes"))
        ));
        assert_eq!(memory.used(), 0);
    }
    for bytes in [
        b"UQAVAL02\x00".as_slice(),
        b"UQAVAL01\xff",
        b"UQAVAL01\x02\x02",
        b"UQAVAL01\x08\x06",
        b"UQAVAL01\x05\x01\0\0\0\0\0\0\0\xff",
    ] {
        assert!(matches!(
            decode_value_image(bytes, &memory, &token),
            Err(ValueImageError::Invalid(_))
        ));
    }
    let combined = [encode(&Value::Int(9)), encode(&Value::Void)].concat();
    let mut input = combined.as_slice();
    assert_eq!(
        *read_value_image(&mut input, &memory, &token).unwrap(),
        Value::Int(9)
    );
    assert_eq!(
        *read_value_image(&mut input, &memory, &token).unwrap(),
        Value::Void
    );
    assert!(input.is_empty());
    assert_eq!(memory.used(), 0);
}

#[test]
fn value_image_structural_corruption_cannot_publish_a_value() {
    let mut images = Vec::new();
    let mut duplicate = encode(&Value::Map(BTreeMap::from([
        ("a".into(), Value::Int(0)),
        ("b".into(), Value::Int(1)),
    ])));
    let b = duplicate.iter().rposition(|byte| *byte == b'b').unwrap();
    duplicate[b] = b'a';
    images.push(duplicate);
    let mut bad_bits = encode(&Value::BitString(
        BitStringValue::try_from_packed(vec![0x80], 1).unwrap(),
    ));
    *bad_bits.last_mut().unwrap() = 0x81;
    images.push(bad_bits);
    let mut bad_key = encode(&Value::Enum(EnumValue::new(
        1,
        EnumLabelKey::from_bytes(vec![1]).unwrap(),
    )));
    *bad_key.last_mut().unwrap() = 0;
    images.push(bad_key);
    let mut bad_dimensions = encode(&Value::Array(
        ArrayValue::try_new(vec![Value::Int(1)]).unwrap(),
    ));
    bad_dimensions[17] = 2;
    images.push(bad_dimensions);
    let mut bad_vector = encode(&Value::LegacyVector(
        LegacyVectorValue::try_new(LegacyVectorKind::SmallInteger, vec![Value::Int(1)]).unwrap(),
    ));
    let offset = bad_vector.len() - 8;
    bad_vector[offset..].copy_from_slice(&32768_i64.to_le_bytes());
    images.push(bad_vector);
    let memory = MemoryBudget::new(1 << 20);
    for image in images {
        assert!(matches!(
            decode_value_image(&image, &memory, &CancellationToken::new()),
            Err(ValueImageError::Invalid(_))
        ));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn value_image_streams_large_payloads_without_encoding_a_second_copy() {
    struct Counter {
        bytes: usize,
        largest: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes += bytes.len();
            self.largest = self.largest.max(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let value = Value::List(vec![
        Value::Str("x".repeat(2 << 20)),
        Value::Bytes(vec![4; 2 << 20]),
        Value::Datum(DatumValue::new(25, u32::MAX, vec![7; 2 << 20])),
    ]);
    let memory = MemoryBudget::new(512);
    let mut counter = Counter {
        bytes: 0,
        largest: 0,
    };
    let allocations = allocation_counter::measure(|| {
        let written =
            write_value_image(&value, &mut counter, &memory, &CancellationToken::new()).unwrap();
        assert_eq!(written as usize, counter.bytes);
    });
    assert!(counter.bytes > 6 << 20);
    assert!(counter.largest <= 16 << 10);
    assert!(allocations.bytes_total < 2048, "{allocations:?}");
    assert_eq!(memory.used(), 0);
    let encoded = encode(&value);
    assert!(matches!(
        decode_value_image(&encoded, &memory, &CancellationToken::new()),
        Err(ValueImageError::Memory(_))
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn value_image_admission_and_cancellation_retire_every_allocation() {
    let value = Value::List(fixtures());
    let bytes = encode(&value);
    for limit in [0, 1, 64, 512, 4096, 1 << 20] {
        let memory = MemoryBudget::new(limit);
        let result = decode_value_image(&bytes, &memory, &CancellationToken::new());
        if limit < 4096 {
            assert!(matches!(result, Err(ValueImageError::Memory(_))));
        }
        if limit == 1 << 20 {
            assert!(result.is_ok());
        }
        drop(result);
        assert_eq!(memory.used(), 0);
        assert!(memory.peak() <= limit);
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    token.cancel();
    let mut output = Vec::new();
    assert!(matches!(
        write_value_image(&value, &mut output, &memory, &token),
        Err(ValueImageError::Cancelled(_))
    ));
    assert!(output.is_empty());
    assert!(matches!(
        decode_value_image(&bytes, &memory, &token),
        Err(ValueImageError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
    for tag in [5, 7, 14, 17] {
        let mut oversized = b"UQAVAL01".to_vec();
        oversized.push(tag);
        oversized.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            decode_value_image(&oversized, &memory, &CancellationToken::new()),
            Err(ValueImageError::Memory(_))
        ));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn value_image_depth_is_bounded_on_both_input_and_output() {
    let mut value = Value::Null;
    for _ in 0..128 {
        value = Value::List(vec![value]);
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    assert!(decode_value_image(&encode(&value), &memory, &token).is_ok());
    let mut invalid = b"UQAVAL01".to_vec();
    for _ in 0..129 {
        invalid.extend_from_slice(b"\x0e\x01\0\0\0\0\0\0\0");
    }
    invalid.push(0);
    assert!(matches!(
        decode_value_image(&invalid, &memory, &token),
        Err(ValueImageError::Invalid("nesting depth"))
    ));
    value = Value::List(vec![value]);
    assert!(matches!(
        write_value_image(&value, &mut std::io::sink(), &memory, &token),
        Err(ValueImageError::Invalid("nesting depth"))
    ));
    assert_eq!(memory.used(), 0);
}

proptest::proptest! {
    #[test]
    fn value_image_float_bits_are_exact(bits in proptest::prelude::any::<u64>()) {
        let memory = MemoryBudget::new(0);
        let token = CancellationToken::new();
        let bytes = encode(&Value::Float(f64::from_bits(bits)));
        let decoded = decode_value_image(&bytes, &memory, &token).unwrap();
        let Value::Float(value) = *decoded else { panic!("float identity") };
        proptest::prop_assert_eq!(value.to_bits(), bits);
    }
}

#[test]
fn value_image_io_errors_and_midstream_cancellation_keep_their_origin() {
    struct Output {
        calls: usize,
        cancellation: CancellationToken,
        cancel: bool,
    }
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 4 {
                if self.cancel {
                    self.cancellation.cancel();
                } else {
                    return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
                }
            }
            Ok(bytes.len().min(31))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    struct Input<'a> {
        bytes: &'a [u8],
        calls: usize,
        cancellation: CancellationToken,
        cancel: bool,
    }
    impl Read for Input<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 4 {
                if self.cancel {
                    self.cancellation.cancel();
                } else {
                    return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
                }
            }
            let length = output.len().min(29);
            self.bytes.read(&mut output[..length])
        }
    }
    let value = Value::Bytes(vec![3; 64 << 10]);
    let bytes = encode(&value);
    for cancel in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut output = Output {
            calls: 0,
            cancellation: token.clone(),
            cancel,
        };
        let result = write_value_image(&value, &mut output, &memory, &token);
        if cancel {
            assert!(matches!(result, Err(ValueImageError::Cancelled(_))));
        } else {
            assert!(
                matches!(result, Err(ValueImageError::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe)
            );
        }
        let token = CancellationToken::new();
        let mut input = Input {
            bytes: &bytes,
            calls: 0,
            cancellation: token.clone(),
            cancel,
        };
        let result = read_value_image(&mut input, &memory, &token);
        if cancel {
            assert!(matches!(result, Err(ValueImageError::Cancelled(_))));
        } else {
            assert!(
                matches!(result, Err(ValueImageError::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied)
            );
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn value_image_borrowed_fields_match_maps_and_reject_invalid_order() {
    let fields = BTreeMap::from([
        (String::new(), Value::Void),
        ("$uqa_type".into(), Value::Str("bytes".into())),
        ("한글".into(), Value::Bytes(vec![0, 255])),
    ]);
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let mut bytes = Vec::new();
    write_fields_image(
        fields.iter().map(|(name, value)| (name.as_str(), value)),
        &mut bytes,
        &memory,
        &token,
    )
    .unwrap();
    assert_eq!(bytes, encode(&Value::Map(fields)));
    for names in [["z", "a"], ["a", "a"]] {
        assert!(matches!(
            write_fields_image(
                names.into_iter().map(|name| (name, &Value::Null)),
                &mut std::io::sink(),
                &memory,
                &token
            ),
            Err(ValueImageError::Invalid("ordered fields"))
        ));
    }
}
