//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};
use std::hash::{Hash, Hasher};

fn packed(text: &str) -> BitStringValue {
    let mut bytes = vec![0_u8; text.len().div_ceil(8)];
    for (position, bit) in text.bytes().enumerate() {
        assert!(matches!(bit, b'0' | b'1'));
        if bit == b'1' {
            bytes[position / 8] |= 0x80 >> (position % 8);
        }
    }
    BitStringValue::try_from_packed(bytes, text.len() as u32).unwrap()
}

#[test]
fn bit_string_order_and_equality_match_postgresql_without_unpacking() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let values = reference["values"].as_array().unwrap();
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let hash = |value: &BitStringValue| {
        let mut state = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut state);
        state.finish()
    };
    for (i, left) in values.iter().enumerate() {
        let left = packed(left.as_str().unwrap());
        for (j, right) in values.iter().enumerate() {
            let right = packed(right.as_str().unwrap());
            let expected = reference["comparisons"][i][j].as_i64().unwrap().cmp(&0);
            assert_eq!(left.cmp(&right), expected);
            assert_eq!(left.cmp_with_control(&right, &control).unwrap(), expected);
            assert_eq!(left == right, expected.is_eq());
            if expected.is_eq() {
                assert_eq!(hash(&left), hash(&right));
            }
        }
    }
    assert_eq!(budget.used(), 0);
    for length in [32_767, 32_768, 32_769, 65_537] {
        let prefix = "0".repeat(length);
        let left = packed(&prefix);
        let right = packed(&(prefix + "1"));
        assert!(left.cmp_with_control(&right, &control).unwrap().is_lt());
        assert!(right.cmp_with_control(&left, &control).unwrap().is_gt());
    }
}

#[test]
fn bit_string_rejects_noncanonical_lengths_and_padding_at_every_constructor() {
    for (bytes, length) in [
        (vec![0x81], 1),
        (vec![0x80, 0], 1),
        (vec![], 1),
        (vec![0], 0),
    ] {
        assert!(BitStringValue::try_from_packed(bytes.clone(), length).is_none());
        let document = serde_json::json!({"bytes":bytes,"bit_len":length});
        assert!(serde_json::from_value::<BitStringValue>(document).is_err());
    }
    for text in ["", "0", "1", "010101010", "11111111111111111"] {
        let value = packed(text);
        let decoded: BitStringValue =
            serde_json::from_value(serde_json::to_value(&value).unwrap()).unwrap();
        assert_eq!(value, decoded);
        assert_eq!(value.bit_len() as usize, text.len());
        assert_eq!(value.is_empty(), text.is_empty());
        for (position, byte) in text.bytes().enumerate() {
            assert_eq!(value.bit(position as u32), Some(byte == b'1'));
        }
        assert_eq!(value.bit(value.bit_len()), None);
    }
}

#[test]
fn bit_string_copy_and_adoption_preserve_allowance_and_cancellation_owners() {
    let value = packed(&"10101010".repeat(4097));
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let budget = MemoryBudget::new(value.as_bytes().len());
    let control = ProductionControl::new(&budget, &original, &invoking);
    let copy = value.clone_with_control(&control).unwrap();
    assert_eq!(*copy, value);
    assert_eq!(copy.reserved_bytes(), value.as_bytes().len());
    drop(copy);
    assert_eq!(budget.used(), 0);

    let tiny = MemoryBudget::new(value.as_bytes().len() - 1);
    let limited = ProductionControl::new(&tiny, &original, &invoking);
    assert!(value.clone_with_control(&limited).is_err());
    assert_eq!(tiny.used(), 0);

    let mut bytes = ProductionVec::new(control);
    bytes.push_copy(0x81).unwrap();
    assert!(
        BitStringValue::try_from_packed_with_control(bytes.finish().unwrap(), 1, &control)
            .unwrap()
            .is_none()
    );
    assert_eq!(budget.used(), 0);

    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(value.clone_with_control(&control).is_err());
        assert!(value.cmp_with_control(&value, &control).is_err());
        assert_eq!(budget.used(), 0);
    }
}
