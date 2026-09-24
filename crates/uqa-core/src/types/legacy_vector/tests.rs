//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken, JsonValueDecoder};

mod arrays;

fn vector(kind: LegacyVectorKind, text: &str) -> LegacyVectorValue {
    LegacyVectorValue::try_new(
        kind,
        text.split_whitespace()
            .map(|value| Value::Int(value.parse().unwrap()))
            .collect(),
    )
    .unwrap()
}

fn key(value: &LegacyVectorValue) -> Vec<u8> {
    let mut bytes = Vec::new();
    value
        .write_comparison_key(|part| {
            bytes.extend_from_slice(part);
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap();
    bytes
}

#[test]
fn ordering_and_keys_match_postgresql_for_both_vector_kinds() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../tests/pg18_legacy_vectors.json")).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    for group in fixture["types"].as_array().unwrap() {
        let kind = if group["type"] == "int2vector" {
            LegacyVectorKind::SmallInteger
        } else {
            LegacyVectorKind::Oid
        };
        let values: Vec<_> = group["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|text| vector(kind, text.as_str().unwrap()))
            .collect();
        for pair in group["comparisons"].as_array().unwrap() {
            let left = &values[pair[0].as_u64().unwrap() as usize];
            let right = &values[pair[1].as_u64().unwrap() as usize];
            let expected = if pair[2] == true {
                Ordering::Equal
            } else if pair[3] == true {
                Ordering::Less
            } else {
                Ordering::Greater
            };
            assert_eq!(pair[4], expected.is_gt());
            assert_eq!(left.cmp(right), expected, "{kind:?}: {left:?}, {right:?}");
            assert_eq!(key(left).cmp(&key(right)), expected);
            assert_eq!(left == right, expected.is_eq());
            assert_eq!(
                Value::LegacyVector(left.clone())
                    .cmp_with_control(&Value::LegacyVector(right.clone()), &control)
                    .unwrap(),
                expected
            );
        }
        for left in &values {
            for middle in &values {
                for right in &values {
                    if left <= middle && middle <= right {
                        assert!(left <= right);
                    }
                }
            }
        }
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn vectors_remain_atomic_array_elements_with_zero_based_empty_views() {
    for kind in [LegacyVectorKind::SmallInteger, LegacyVectorKind::Oid] {
        let empty = vector(kind, "");
        assert_eq!(empty.as_array().dimensions(), &[0]);
        assert_eq!(empty.as_array().lower_bound(0), Some(0));
        assert_eq!(empty.as_array().upper_bound(0), Some(-1));
        let values = vec![
            Value::LegacyVector(empty),
            Value::LegacyVector(vector(kind, "1 2")),
        ];
        let outer = ArrayValue::try_new(values.clone()).unwrap();
        assert_eq!(outer.dimensions(), &[2]);
        assert_eq!(outer.lower_bounds(), &[1]);
        assert_eq!(outer.elements(), values);
        assert_eq!(
            outer.flattened_elements().collect::<Vec<_>>(),
            values.iter().collect::<Vec<_>>()
        );
        let encoded = serde_json::to_string(&Value::Array(outer)).unwrap();
        let Value::Array(decoded) = serde_json::from_str(&encoded).unwrap() else {
            panic!("outer SQL array");
        };
        assert_eq!(decoded.dimensions(), &[2]);
        assert_eq!(decoded.elements(), values);
    }
    let ordinary_empty = ArrayValue::try_new(Vec::new()).unwrap();
    assert!(ordinary_empty.dimensions().is_empty());
    assert_eq!(ordinary_empty.upper_bound(0), None);
    let bounded_empty = ArrayValue::with_lower_bounds(Vec::new(), vec![0]).unwrap();
    assert_eq!(bounded_empty.dimensions(), &[0]);
    assert_eq!(bounded_empty.upper_bound(0), Some(-1));
    let encoded = serde_json::to_string(&bounded_empty).unwrap();
    assert_eq!(
        serde_json::from_str::<ArrayValue>(&encoded).unwrap(),
        bounded_empty
    );
}

#[test]
fn tagged_decoding_and_copying_retain_kind_shape_and_exact_payload_leases() {
    let cases = [
        r#"{"$uqa_type":"int2vector","values":[]}"#,
        r#"{"$uqa_type":"int2vector","values":[-32768,0,32767]}"#,
        r#"{"$uqa_type":"oidvector","values":[]}"#,
        r#"{"$uqa_type":"oidvector","values":[0,4294967295]}"#,
        r#"{"$uqa_type":"array","values":[],"lower_bounds":[0]}"#,
    ];
    for encoded in cases {
        let memory = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let decoded = JsonValueDecoder::new(&memory, &cancellation)
            .value(encoded)
            .unwrap();
        let array = match &*decoded {
            Value::LegacyVector(vector) => {
                assert_eq!(
                    serde_json::from_str::<LegacyVectorValue>(encoded).unwrap(),
                    *vector
                );
                vector.as_array()
            }
            Value::Array(array) => array,
            _ => panic!("recognized typed value"),
        };
        assert_eq!(array.dimensions(), &[array.elements().len()]);
        assert_eq!(array.lower_bounds(), &[0]);
        assert_eq!(
            decoded.reserved_bytes(),
            decoded
                .retained_payload_bytes(&memory, &cancellation)
                .unwrap()
        );
        let copied = decoded.clone_budgeted(&memory, &cancellation).unwrap();
        assert_eq!(*decoded, *copied);
        assert_eq!(
            serde_json::to_value(&*decoded).unwrap(),
            serde_json::from_str::<serde_json::Value>(encoded).unwrap()
        );
        assert_eq!(
            memory.used(),
            decoded.reserved_bytes() + copied.reserved_bytes()
        );
        drop((decoded, copied));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn invalid_carriers_and_tags_preserve_type_and_width_invariants() {
    for (kind, invalid) in [
        (LegacyVectorKind::SmallInteger, Value::Int(32768)),
        (LegacyVectorKind::SmallInteger, Value::Null),
        (
            LegacyVectorKind::SmallInteger,
            Value::List(vec![Value::Int(1)]),
        ),
        (LegacyVectorKind::Oid, Value::Int(-1)),
        (LegacyVectorKind::Oid, Value::Int(4_294_967_296)),
        (LegacyVectorKind::Oid, Value::Float(1.0)),
    ] {
        let encoded =
            serde_json::json!({"$uqa_type":kind.type_name(),"values":[invalid]}).to_string();
        assert!(LegacyVectorValue::try_new(kind, vec![invalid]).is_none());
        assert!(matches!(
            serde_json::from_str::<Value>(&encoded).unwrap(),
            Value::Map(_)
        ));
        assert!(serde_json::from_str::<LegacyVectorValue>(&encoded).is_err());
        let memory = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let decoded = JsonValueDecoder::new(&memory, &cancellation)
            .value(&encoded)
            .unwrap();
        assert_eq!(
            serde_json::to_value(&*decoded).unwrap(),
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap()
        );
        drop(decoded);
        assert_eq!(memory.used(), 0);
    }
    assert_ne!(
        vector(LegacyVectorKind::SmallInteger, "1 2"),
        vector(LegacyVectorKind::Oid, "1 2")
    );
}

#[test]
fn production_and_copy_failures_release_their_shared_allowance() {
    let kind = LegacyVectorKind::Oid;
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut elements = ProductionVec::new(control);
        elements
            .push_produced(control.copy_value(&Value::Int(7)).unwrap())
            .unwrap();
        let elements = elements.finish().unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            LegacyVectorValue::try_new_with_control(kind, elements, &control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let elements = ProductionVec::new(control).finish().unwrap();
    assert!(matches!(
        LegacyVectorValue::try_new_with_control(kind, elements, &control),
        Err(ValueRetentionError::Memory(_))
    ));
    let value = Value::LegacyVector(vector(kind, "1 2"));
    assert!(matches!(
        value.clone_budgeted(&memory, &cancellation),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(memory.used(), 0);
}
