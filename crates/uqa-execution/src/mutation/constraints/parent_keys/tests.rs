//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::ParentKeyBuffer;
use std::collections::BTreeSet;
use uqa_core::{CancellationToken, DecimalValue, Value};

fn wide_key(index: usize) -> Vec<Value> {
    vec![
        Value::Int(i64::try_from(index).unwrap()),
        Value::Str("κ".repeat(2048)),
    ]
}

#[test]
fn deferred_parent_keys_compare_physical_and_logical_values_before_spilling() {
    let physical = Value::Datum(uqa_core::DatumValue::new(
        23,
        0,
        7_i32.to_le_bytes().to_vec(),
    ));
    for allowance in [0, 64 * 1024] {
        for value in [Value::Int(7), physical.clone()] {
            let mut builder = ParentKeyBuffer::new(allowance, CancellationToken::new());
            builder.insert(&[value], None).unwrap();
            let keys = builder.finish().unwrap();
            assert!(keys.contains(&[Value::Int(7)], None).unwrap());
            assert!(keys
                .contains(std::slice::from_ref(&physical), None)
                .unwrap());
            assert!(!keys.contains(&[Value::Int(8)], None).unwrap());
        }
    }
}

#[test]
fn deferred_parent_keys_bound_wide_retention_and_keep_exact_membership() {
    const COUNT: usize = 2048;
    let former = allocation_counter::measure(|| {
        let mut keys = BTreeSet::new();
        for index in 0..COUNT {
            keys.insert(wide_key(index));
        }
        assert!(keys.contains(&wide_key(COUNT - 1)));
        assert!(!keys.contains(&wide_key(COUNT)));
    });
    assert!(former.bytes_max > 4 * 1024 * 1024, "{former:?}");
    let bounded = allocation_counter::measure(|| {
        let mut keys = ParentKeyBuffer::new(64 * 1024, CancellationToken::new());
        for index in (0..COUNT).rev() {
            keys.insert(&wide_key(index), None).unwrap();
        }
        keys.insert(&wide_key(17), None).unwrap();
        let keys = keys.finish().unwrap();
        for index in (0..COUNT).step_by(31) {
            let before = keys.reads.get();
            assert!(keys.contains(&wide_key(index), None).unwrap());
            assert!(keys.reads.get() - before <= 12);
            let mut different = wide_key(index);
            different[1] = Value::Str("λ".repeat(2048));
            assert!(!keys.contains(&different, None).unwrap());
        }
        assert!(!keys.contains(&wide_key(COUNT), None).unwrap());
    });
    assert!(bounded.bytes_max <= 512 * 1024, "{bounded:?}");
}

#[test]
fn deferred_parent_key_spill_preserves_core_normalized_equality() {
    let inputs = [
        vec![Value::Int(1)],
        vec![Value::Float(-0.0)],
        vec![Value::Float(f64::NAN)],
        vec![Value::FixedChar("x  ".into())],
        vec![Value::JsonB("{\"a\":1,\"b\":2}".into())],
        vec![Value::Record(vec![("first".into(), Value::Int(1))].into())],
        vec![Value::Null, Value::Str("x".into())],
    ];
    let mut probes = inputs.to_vec();
    probes.extend([
        vec![Value::Decimal(DecimalValue::from_i64(1))],
        vec![Value::Bool(true)],
        vec![Value::Float(1.0)],
        vec![Value::Float(0.0)],
        vec![Value::Float(f64::from_bits(0x7ff8_0000_0000_0001))],
        vec![Value::FixedChar("x".into())],
        vec![Value::JsonB("{\"b\":2,\"a\":1.0}".into())],
        vec![Value::Record(
            vec![("renamed".into(), Value::Int(1))].into(),
        )],
        vec![Value::Int(2)],
        vec![Value::Str("x".into())],
        vec![Value::Null],
    ]);
    let expected: BTreeSet<_> = inputs.iter().cloned().collect();
    for allowance in [0, 1, 64 * 1024] {
        let mut keys = ParentKeyBuffer::new(allowance, CancellationToken::new());
        for key in &inputs {
            keys.insert(key, None).unwrap();
        }
        let keys = keys.finish().unwrap();
        for key in &probes {
            assert_eq!(
                keys.contains(key, None).unwrap(),
                expected.contains(key),
                "{key:?}"
            );
        }
    }
}

#[test]
fn deferred_parent_key_digest_collisions_require_exact_owned_keys() {
    for allowance in [0, 64 * 1024] {
        let mut keys = ParentKeyBuffer::new(allowance, CancellationToken::new());
        for key in [b"a".to_vec(), b"b".to_vec(), b"a".to_vec()] {
            keys.insert_encoded(key, [0; 32]).unwrap();
        }
        let keys = keys.finish().unwrap();
        assert!(keys.contains_encoded(b"a", [0; 32]).unwrap());
        assert!(keys.contains_encoded(b"b", [0; 32]).unwrap());
        assert!(!keys.contains_encoded(b"c", [0; 32]).unwrap());
        assert!(!keys.contains_encoded(b"a", [1; 32]).unwrap());
    }
}

#[test]
fn deferred_parent_keys_retain_cancellation_and_spill_oversized_keys() {
    let cancellation = CancellationToken::new();
    let mut builder = ParentKeyBuffer::new(0, cancellation.clone());
    builder.insert(&[Value::Int(1)], None).unwrap();
    let keys = builder.finish().unwrap();
    let mut builder = ParentKeyBuffer::new(0, cancellation.clone());
    cancellation.cancel();
    assert_eq!(
        keys.contains(&[Value::Int(1)], None)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(
        builder
            .insert(&[Value::Int(2)], None)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    let mut builder = ParentKeyBuffer::new(0, CancellationToken::new());
    let key = vec![Value::Str("x".repeat(1024 * 1024))];
    builder.insert(&key, None).unwrap();
    let keys = builder.finish().unwrap();
    assert!(keys.contains(&key, None).unwrap());
    assert!(!keys
        .contains(&[Value::Str("x".repeat(1024 * 1024 - 1))], None)
        .unwrap());
}
