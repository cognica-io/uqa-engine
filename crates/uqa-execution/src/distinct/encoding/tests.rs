//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::ArrayValue;

#[test]
fn controlled_keys_keep_all_canonical_domains_and_only_retain_the_output_buffer() {
    let control = StorageReadControl::with_limit(8 * 1024 * 1024);
    let values = vec![
        Value::Null,
        Value::Void,
        Value::Bool(true),
        Value::Int(i64::MIN),
        Value::Int(i64::MAX),
        Value::Float(-0.0),
        Value::Float(f64::MAX),
        Value::Float(f64::from_bits(1)),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(f64::NAN),
        Value::Decimal(DecimalValue::parse("-12.34000").unwrap()),
        Value::Str("a\0é".into()),
        Value::FixedChar("padded   ".into()),
        Value::Bytes(vec![0, 255]),
        Value::Json(" {\"x\":1} ".into()),
        Value::JsonB("{\"x\":1.00,\"a\":[true,null]}".into()),
        Value::Temporal(TemporalValue::TimeTz {
            micros: 3_600_000_000,
            offset_minutes: 60,
        }),
        Value::Array(
            ArrayValue::with_lower_bounds(vec![Value::Int(1), Value::Null], vec![-2]).unwrap(),
        ),
        Value::List(vec![Value::Bool(true)]),
        Value::Row(vec![Value::Int(2)]),
        Value::Record(vec![("ignored name".into(), Value::Int(3))]),
        Value::Map(std::collections::BTreeMap::from([(
            "name".into(),
            Value::Int(4),
        )])),
    ];
    for value in &values {
        let key = canonical_row_key_budgeted(std::iter::once(Some(value)), &control).unwrap();
        assert_eq!(
            &*key,
            canonical_row_key(std::slice::from_ref(value)).unwrap()
        );
        assert_eq!(control.memory().used(), key.capacity());
        drop(key);
        assert_eq!(control.memory().used(), 0);
    }
    let null = canonical_row_key_budgeted(std::iter::once(None), &control).unwrap();
    assert_eq!(&*null, canonical_row_key(&[Value::Null]).unwrap());
}

#[test]
fn controlled_numeric_domains_keep_the_persisted_integer_bytes_and_signed_zero() {
    let control = StorageReadControl::with_limit(1024 * 1024);
    let expected = [0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, b'1'];
    for value in [
        Value::Bool(true),
        Value::Int(1),
        Value::Float(1.0),
        Value::Decimal(DecimalValue::parse("1.000").unwrap()),
    ] {
        let key = canonical_row_key_budgeted(std::iter::once(Some(&value)), &control).unwrap();
        assert_eq!(&*key, expected);
    }
    assert_eq!(
        canonical_row_key(&[Value::Float(-0.0)]).unwrap(),
        canonical_row_key(&[Value::Int(0)]).unwrap()
    );
}

#[test]
fn normalization_failures_keep_memory_and_cancellation_diagnostics() {
    for (label, value) in [
        ("text", Value::Str("x".repeat(8192))),
        ("jsonb", Value::JsonB(format!("[\"{}\"]", "x".repeat(8192)))),
        (
            "decimal",
            Value::Decimal(DecimalValue::parse("1e131071").unwrap()),
        ),
        ("float", Value::Float(f64::from_bits(1))),
    ] {
        let control = StorageReadControl::with_limit(256);
        let error =
            canonical_row_key_budgeted(std::iter::once(Some(&value)), &control).unwrap_err();
        assert!(
            matches!(error, ExecError::SQL(ref error) if error.sqlstate() == Some("53200")),
            "{label}: {error}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(256);
    control.cancellation().cancel();
    assert!(matches!(
        canonical_row_key_budgeted(std::iter::once(Some(&Value::Int(1))), &control),
        Err(ExecError::SQL(uqa_sql::SQLError::Cancelled(_)))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn canonical_numeric_keys_match_value_equality_at_precision_boundaries() {
    let decimal = |text| Value::Decimal(DecimalValue::parse(text).unwrap());
    let values = [
        Value::Int(9_223_372_036_854_774_784),
        Value::Float(9_223_372_036_854_774_784_i64 as f64),
        decimal("9223372036854774784"),
        decimal("9223372036854775000"),
        Value::Float(0.1),
        decimal("0.1"),
        decimal("0.1000000000000000055511151231257827021181583404541015625"),
        Value::Float(f64::from_bits(1)),
        Value::Int(0),
        Value::Float(-0.0),
        decimal("0.000"),
        Value::Float(f64::NAN),
        decimal("NaN"),
        Value::Float(f64::INFINITY),
        decimal("Infinity"),
        Value::Float(f64::NEG_INFINITY),
        decimal("-Infinity"),
    ];
    let control = StorageReadControl::with_limit(64 * 1024);
    let hash_state = std::collections::hash_map::RandomState::new();
    for left in &values {
        let left_key = canonical_row_key(std::slice::from_ref(left)).unwrap();
        let controlled = canonical_row_key_budgeted(std::iter::once(Some(left)), &control).unwrap();
        assert_eq!(&*controlled, &left_key);
        drop(controlled);
        assert_eq!(control.memory().used(), 0);
        for right in &values {
            let right_key = canonical_row_key(std::slice::from_ref(right)).unwrap();
            assert_eq!(left_key == right_key, left == right, "{left:?}, {right:?}");
            if left == right {
                assert_eq!(
                    hash_canonical_row(&hash_state, std::iter::once(Some(left))).unwrap(),
                    hash_canonical_row(&hash_state, std::iter::once(Some(right))).unwrap()
                );
            }
            let left_row = Value::Row(vec![left.clone(), Value::Null]);
            let right_row = Value::Row(vec![right.clone(), Value::Null]);
            assert_eq!(
                canonical_row_key(std::slice::from_ref(&left_row)).unwrap()
                    == canonical_row_key(std::slice::from_ref(&right_row)).unwrap(),
                left_row == right_row
            );
        }
    }
}

#[test]
fn nested_values_use_a_charged_traversal_stack_without_cloning_their_payload() {
    let control = StorageReadControl::with_limit(1024 * 1024);
    let mut value = Value::Str("original payload".into());
    for _ in 0..256 {
        value = Value::List(vec![value]);
    }
    let key = canonical_row_key_budgeted(std::iter::once(Some(&value)), &control).unwrap();
    assert_eq!(
        &*key,
        canonical_row_key(std::slice::from_ref(&value)).unwrap()
    );
    assert_eq!(control.memory().used(), key.capacity());
}
