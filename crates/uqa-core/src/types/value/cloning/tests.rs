//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{collections::BTreeMap, hint::black_box};

use crate::{
    ArrayValue, BitStringValue, DatumValue, DecimalValue, EnumLabelKey, EnumValue,
    LegacyVectorKind, LegacyVectorValue, RecordFieldType, RecordValue, RowValue, TemporalValue,
    Value,
};

#[test]
fn scalar_clones_preserve_bits_and_temporal_fields_without_allocating() {
    let values = [
        Value::Null,
        Value::Void,
        Value::Bool(false),
        Value::Bool(true),
        Value::Int(i64::MIN),
        Value::Int(i64::MAX),
        Value::Float(-0.0),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(f64::from_bits(0x7ff0_0000_0000_0123)),
        Value::Float(f64::from_bits(0xfff8_0000_0000_0456)),
        Value::Temporal(TemporalValue::Date { days: -42 }),
        Value::Temporal(TemporalValue::Time { micros: 123 }),
        Value::Temporal(TemporalValue::TimeTz {
            micros: 456,
            offset_minutes: -570,
        }),
        Value::Temporal(TemporalValue::Timestamp { micros: -789 }),
        Value::Temporal(TemporalValue::TimestampTz { micros: i64::MAX }),
        Value::Temporal(TemporalValue::Interval {
            months: 2,
            days: -60,
            micros: 1,
        }),
    ];
    let allocation = allocation_counter::measure(|| {
        for value in &values {
            let cloned = black_box(value).clone();
            assert!(value.has_same_representation(black_box(&cloned)));
        }
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(allocation.bytes_total, 0);
}

#[test]
fn payload_clones_preserve_owned_buffers_and_nested_type_metadata() {
    let values = [
        Value::Str("owned text".into()),
        Value::FixedChar("padded  ".into()),
        Value::Bytes(vec![0, 255, 17]),
        Value::Decimal(DecimalValue::parse("12.3400").unwrap()),
        Value::Json(" { \"n\" : 1 } ".into()),
        Value::JsonB("{\"n\":1}".into()),
        Value::Array(
            ArrayValue::with_lower_bounds(vec![Value::Int(7), Value::Null], vec![-3])
                .unwrap()
                .with_element_type_oid(Some(90001)),
        ),
        Value::LegacyVector(
            LegacyVectorValue::try_new(LegacyVectorKind::Oid, vec![Value::Int(17)]).unwrap(),
        ),
        Value::List(vec![Value::Str("nested".into()), Value::Float(-0.0)]),
        Value::Row(
            RowValue::typed(
                vec![Value::Str("collated".into())],
                vec![RecordFieldType {
                    oid: 25,
                    type_modifier: 11,
                    collation_oid: Some(950),
                }],
            )
            .unwrap(),
        ),
        Value::Record(RecordValue::from_parts(
            vec![("field".into(), Value::Float(-0.0))],
            Some(90002),
        )),
        Value::Map(BTreeMap::from([("key".into(), Value::Str("value".into()))])),
        Value::Enum(
            EnumValue::new(90003, EnumLabelKey::from_bytes(vec![64, 1]).unwrap())
                .with_label_oid(Some(90004)),
        ),
        Value::BitString(BitStringValue::try_from_packed(vec![0b1010_0000], 3).unwrap()),
    ];
    for value in &values {
        let cloned = value.clone();
        assert!(value.has_same_representation(&cloned));
        match (value, &cloned) {
            (Value::Str(a), Value::Str(b))
            | (Value::FixedChar(a), Value::FixedChar(b))
            | (Value::Json(a), Value::Json(b))
            | (Value::JsonB(a), Value::JsonB(b)) => assert_ne!(a.as_ptr(), b.as_ptr()),
            (Value::Bytes(a), Value::Bytes(b)) => assert_ne!(a.as_ptr(), b.as_ptr()),
            (Value::Array(a), Value::Array(b)) => {
                assert_ne!(a.elements().as_ptr(), b.elements().as_ptr());
            }
            (Value::LegacyVector(a), Value::LegacyVector(b)) => {
                assert_ne!(a.elements().as_ptr(), b.elements().as_ptr());
            }
            (Value::List(a), Value::List(b)) => assert_ne!(a.as_ptr(), b.as_ptr()),
            (Value::Row(a), Value::Row(b)) => {
                assert_ne!(a.values().as_ptr(), b.values().as_ptr());
                assert_ne!(
                    a.field_types().unwrap().as_ptr(),
                    b.field_types().unwrap().as_ptr()
                );
            }
            (Value::Record(a), Value::Record(b)) => assert_ne!(a.as_ptr(), b.as_ptr()),
            (Value::Map(a), Value::Map(b)) => {
                assert!(!std::ptr::eq(&raw const a["key"], &raw const b["key"]));
            }
            (Value::Enum(a), Value::Enum(b)) => {
                assert_ne!(a.key().as_bytes().as_ptr(), b.key().as_bytes().as_ptr());
            }
            (Value::BitString(a), Value::BitString(b)) => {
                assert_ne!(a.as_bytes().as_ptr(), b.as_bytes().as_ptr());
            }
            _ => {}
        }
        let mut replaced = Value::Str("previous owner".into());
        replaced.clone_from(value);
        assert!(value.has_same_representation(&replaced));
    }
}

#[test]
fn datum_cloning_shares_opaque_bytes_without_interpreting_the_offset() {
    let datum = DatumValue::new(90005, u32::MAX, vec![1, 2, 3]);
    let original = Value::Datum(datum);
    let allocation = allocation_counter::measure(|| {
        let Value::Datum(cloned) = black_box(&original).clone() else {
            panic!("retained Datum variant changed");
        };
        let Value::Datum(source) = &original else {
            unreachable!();
        };
        assert_eq!(cloned.type_oid(), 90005);
        assert_eq!(cloned.offset(), u32::MAX);
        assert_eq!(cloned.bytes(), &[1, 2, 3]);
        assert_eq!(cloned.bytes().as_ptr(), source.bytes().as_ptr());
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(allocation.bytes_total, 0);
}
