//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::btree_index::equality;
use uqa_core::BitStringValue;

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(usize::MAX)
}

fn row(values: Vec<Value>) -> Value {
    Value::Row(values.into())
}

fn bits(bytes: Vec<u8>, length: u32) -> Value {
    Value::BitString(BitStringValue::try_from_packed(bytes, length).unwrap())
}

/// Values of every kind of the domain, with those that only an exact comparison tells apart.
fn domain() -> Vec<Value> {
    let two_to_53 = 9_007_199_254_740_992_i64;
    let mut values = vec![
        Value::Null,
        Value::Bool(false),
        Value::Bool(true),
        Value::Str(String::new()),
        Value::Str("a".into()),
        Value::Str("a\0".into()),
        Value::Str("a\0b".into()),
        Value::Str("a\u{1}".into()),
        Value::Str("ab".into()),
        Value::Str("b".into()),
        Value::Str("\u{d55c}\u{ae00}".into()),
        Value::Bytes(Vec::new()),
        Value::Bytes(vec![0]),
        Value::Bytes(vec![0, 0]),
        Value::Bytes(vec![0, 255]),
        Value::Bytes(vec![1]),
        Value::Bytes(vec![255]),
        Value::Bytes(vec![255, 0]),
        bits(Vec::new(), 0),
        bits(vec![0x00], 1),
        bits(vec![0x00], 8),
        bits(vec![0x80], 1),
        bits(vec![0x80], 8),
        bits(vec![0x80, 0x00], 9),
        row(Vec::new()),
        row(vec![Value::Null]),
        row(vec![Value::Int(1)]),
        row(vec![Value::Int(1), Value::Null]),
        row(vec![Value::Int(1), Value::Int(2)]),
        row(vec![Value::Float(1.0), Value::Str("a".into())]),
        row(vec![Value::Int(1), Value::Str("a".into()), Value::Null]),
        row(vec![Value::Null, Value::Int(1)]),
        row(vec![Value::Str("a".into())]),
        row(vec![Value::Str("a\0".into()), Value::Int(0)]),
        row(vec![row(vec![Value::Int(1)]), Value::Int(2)]),
        row(vec![row(vec![Value::Int(1), Value::Int(2)])]),
        row(vec![row(Vec::new())]),
    ];
    for integer in [
        i64::MIN,
        i64::MIN + 1,
        -two_to_53 - 1,
        -two_to_53,
        -1000,
        -2,
        -1,
        0,
        1,
        2,
        3,
        1000,
        two_to_53 - 1,
        two_to_53,
        two_to_53 + 1,
        i64::MAX - 1,
        i64::MAX,
    ] {
        values.push(Value::Int(integer));
    }
    for float in [
        f64::NEG_INFINITY,
        f64::MIN,
        -1e300,
        -9_223_372_036_854_775_808.0,
        -9_007_199_254_740_993.0,
        -1000.5,
        -2.5,
        -2.0,
        -1.0,
        -0.5,
        -f64::MIN_POSITIVE,
        -5e-324,
        -0.0,
        0.0,
        5e-324,
        1e-320,
        f64::MIN_POSITIVE,
        0.5,
        1.0,
        1.5,
        2.0,
        2.5,
        1000.0,
        9_007_199_254_740_992.0,
        9_223_372_036_854_775_808.0,
        1e300,
        f64::MAX,
        f64::INFINITY,
    ] {
        values.push(Value::Float(float));
    }
    values
}

#[test]
fn keys_order_and_equal_as_the_values_do() {
    let control = control();
    let values = domain();
    let keys: Vec<_> = values
        .iter()
        .map(|value| {
            key(value, &control)
                .unwrap()
                .expect("a value of the domain")
        })
        .collect();
    let equal: Vec<_> = values
        .iter()
        .map(|value| {
            equality::equal_key(value, &control)
                .unwrap()
                .expect("a value of the domain")
        })
        .collect();
    for (left, left_value) in values.iter().enumerate() {
        for (right, right_value) in values.iter().enumerate() {
            assert_eq!(
                keys[left].cmp(&keys[right]),
                left_value.cmp(right_value),
                "{left_value:?} and {right_value:?}"
            );
            // The earlier equality keys tell the same values apart.
            assert_eq!(
                *keys[left] == *keys[right],
                *equal[left] == *equal[right],
                "{left_value:?} and {right_value:?}"
            );
        }
    }
    drop((keys, equal));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn no_key_is_the_beginning_of_another() {
    let control = control();
    let keys: Vec<_> = domain()
        .iter()
        .map(|value| key(value, &control).unwrap().unwrap())
        .collect();
    for left in &keys {
        for right in &keys {
            assert!(
                **left == **right || !right.starts_with(left),
                "{left:?} begins {right:?}"
            );
        }
    }
}

#[test]
fn a_value_outside_the_domain_has_no_key() {
    let control = control();
    for value in [
        Value::Float(f64::NAN),
        Value::FixedChar("a  ".into()),
        Value::Temporal(uqa_core::TemporalValue::parse_date("2024-01-01").unwrap()),
        Value::Decimal(uqa_core::DecimalValue::parse("1.5").unwrap()),
        Value::Json("{}".into()),
        row(vec![Value::Int(1), Value::Float(f64::NAN)]),
        row(vec![row(vec![Value::FixedChar("a".into())])]),
    ] {
        assert!(key(&value, &control).unwrap().is_none(), "{value:?}");
        assert!(
            equality::equal_key(&value, &control).unwrap().is_none(),
            "{value:?}"
        );
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn a_key_is_charged_to_the_caller_and_stops_with_it() {
    let value = row(vec![Value::Str("x".repeat(4096)), Value::Int(1)]);
    let limited = StorageReadControl::with_limit(64);
    assert!(key(&value, &limited).is_err());
    assert_eq!(limited.memory().used(), 0);
    let cancelled = control();
    cancelled.cancellation().cancel();
    assert!(key(&value, &cancelled).is_err());
    assert_eq!(cancelled.memory().used(), 0);
}
