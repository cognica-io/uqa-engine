//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicate intervals must include exactly the keys selected by the real value index, including future inserts into an empty index.

use super::*;
use crate::catalog::index::value::ColumnValueIndex;
use proptest::prelude::*;

fn decimal(text: &str) -> Value {
    Value::Decimal(DecimalValue::parse(text).unwrap())
}

fn contains(range: &IndexKeyRange, key: &[u8]) -> bool {
    let (lower, upper) = range.bounds();
    let above = match lower {
        Bound::Included(lower) => key >= lower,
        Bound::Excluded(lower) => key > lower,
        Bound::Unbounded => true,
    };
    let below = match upper {
        Bound::Included(upper) => key <= upper,
        Bound::Excluded(upper) => key < upper,
        Bound::Unbounded => true,
    };
    above && below
}

fn observed(domain: ScalarIndexDomain, predicate: &Predicate, value: &Value) -> bool {
    let control = StorageReadControl::with_limit(1 << 20);
    let key = domain.encode(value, &control).unwrap();
    let mut found = false;
    domain
        .visit_predicate(predicate, &control, &mut |range| {
            found |= contains(&range, &key);
            Ok(())
        })
        .unwrap();
    found
}

fn verify(domain: ScalarIndexDomain, values: &[Value], targets: &[Value]) {
    let empty = ColumnValueIndex::build("key", std::iter::empty());
    let mut predicates = vec![Predicate::IsNull, Predicate::IsNotNull];
    for target in targets {
        predicates.extend([
            Predicate::Equals(target.clone()),
            Predicate::GreaterThan(target.clone()),
            Predicate::GreaterThanOrEqual(target.clone()),
            Predicate::LessThan(target.clone()),
            Predicate::LessThanOrEqual(target.clone()),
            Predicate::Between {
                low: target.clone(),
                high: target.clone(),
            },
        ]);
    }
    predicates.push(Predicate::InSet(targets.iter().cloned().collect()));
    for predicate in predicates {
        if empty.scan(&predicate).is_none() {
            continue;
        }
        assert!(empty.scan(&predicate).unwrap().is_empty());
        for value in values {
            let index = ColumnValueIndex::build("key", std::iter::once((1, value.clone())));
            let selected = !index.scan(&predicate).unwrap().is_empty();
            assert_eq!(
                observed(domain, &predicate, value),
                selected,
                "{domain:?}, {predicate:?}, {value:?}"
            );
        }
    }
    let control = StorageReadControl::with_limit(1 << 20);
    for left in values {
        for right in values {
            let a = domain.encode(left, &control).unwrap();
            let b = domain.encode(right, &control).unwrap();
            assert_eq!(
                a.as_ref().cmp(b.as_ref()),
                left.cmp(right),
                "{domain:?}, {left:?}, {right:?}"
            );
        }
    }
}

#[test]
fn finite_numeric_domains_preserve_fractional_and_large_mixed_bounds() {
    let targets = [
        Value::Null,
        Value::Int(i64::MIN),
        Value::Int(-1),
        Value::Int(0),
        Value::Int(1),
        Value::Int(i64::MAX),
        Value::Int(9_223_372_036_854_774_784),
        Value::Float(-0.5),
        Value::Float(0.5),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        decimal("9223372036854775000"),
        decimal("0.1"),
        decimal("-0.1"),
        decimal("NaN"),
    ];
    verify(
        ScalarIndexDomain::Integer,
        &[
            Value::Null,
            Value::Int(i64::MIN),
            Value::Int(i64::MIN + 1),
            Value::Int(-1),
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(9_223_372_036_854_774_784),
            Value::Int(i64::MAX - 1),
            Value::Int(i64::MAX),
        ],
        &targets,
    );
    verify(
        ScalarIndexDomain::Boolean,
        &[Value::Null, Value::Bool(false), Value::Bool(true)],
        &targets,
    );
    verify(
        ScalarIndexDomain::Float,
        &[
            Value::Null,
            Value::Float(f64::NEG_INFINITY),
            Value::Float(-1.0),
            Value::Float(-0.1),
            Value::Float(-0.0),
            Value::Float(0.0),
            Value::Float(f64::from_bits(1)),
            Value::Float(0.1),
            Value::Float(1.0),
            Value::Float(9_223_372_036_854_774_784_i64 as f64),
            Value::Float(i64::MAX as f64),
            Value::Float(f64::INFINITY),
            Value::Float(f64::NAN),
        ],
        &targets,
    );
}

#[test]
fn decimal_keys_preserve_exact_order_and_display_scale_equivalence() {
    let mut values = vec![Value::Null];
    values.extend(
        [
            "-Infinity",
            "-1e1000",
            "-1200.000",
            "-1.01",
            "-1.00",
            "-0.001",
            "0",
            "0.00",
            "0.00100",
            "1.00",
            "1.01",
            "1200",
            "1e1000",
            "Infinity",
            "NaN",
        ]
        .map(decimal),
    );
    let mut targets = values.clone();
    targets.extend([
        Value::Int(0),
        Value::Int(1),
        Value::Float(0.1),
        Value::Float(9_223_372_036_854_774_784_i64 as f64),
        Value::Str("x".into()),
    ]);
    verify(ScalarIndexDomain::Decimal, &values, &targets);
}

#[test]
fn byte_and_text_domains_preserve_prefixes_embedded_zeros_and_padding() {
    for (domain, values) in [
        (
            ScalarIndexDomain::Text,
            ["", "\0", "a", "a\0", "a\0x", "aa", "b", "한"]
                .map(|text| Value::Str(text.into()))
                .to_vec(),
        ),
        (
            ScalarIndexDomain::FixedChar,
            ["", " ", "a", "a ", "a\0 ", "aa", "b", "한 "]
                .map(|text| Value::FixedChar(text.into()))
                .to_vec(),
        ),
        (
            ScalarIndexDomain::Bytes,
            [
                b"".as_slice(),
                b"\0",
                b"\0\xff",
                b"a",
                b"a\0",
                b"aa",
                b"\xff",
                b"\xff\0",
            ]
            .map(|bytes| Value::Bytes(bytes.to_vec()))
            .to_vec(),
        ),
        (
            ScalarIndexDomain::JsonText,
            ["0", "1", "10", "2", "null", "[]", "{}", "\"a\""]
                .map(|text| Value::Json(text.into()))
                .to_vec(),
        ),
    ] {
        let mut values = values;
        values.push(Value::Null);
        let mut targets = values.clone();
        targets.extend([
            Value::Int(0),
            Value::Str("a".into()),
            Value::FixedChar("a".into()),
        ]);
        verify(domain, &values, &targets);
    }
}

#[test]
fn empty_and_open_intervals_keep_their_exact_endpoints() {
    for (low, high) in [(1, 2), (2, 1), (i64::MIN, i64::MAX)] {
        let predicate = Predicate::Between {
            low: Value::Int(low),
            high: Value::Int(high),
        };
        for key in [i64::MIN, 0, 1, 2, 3, i64::MAX] {
            assert_eq!(
                observed(ScalarIndexDomain::Integer, &predicate, &Value::Int(key)),
                (low..=high).contains(&key)
            );
        }
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let mut visited = false;
    let error = ScalarIndexDomain::Integer
        .visit_predicate(&Predicate::NotEquals(Value::Int(1)), &control, &mut |_| {
            visited = true;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert!(!visited);
}

#[test]
fn keys_share_the_original_allowance_and_fail_cleanly_on_cancellation_or_limit() {
    let control = StorageReadControl::with_limit(64);
    let error = ScalarIndexDomain::Text
        .encode(&Value::Str("x".repeat(1024)), &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(control.memory().used(), 0);
    let key = ScalarIndexDomain::Integer
        .encode(&Value::Int(1), &control)
        .unwrap();
    assert!(key.budget().shares_allowance(control.memory()));
    assert!(control.memory().used() > 0);
    drop(key);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    let mut visited = false;
    let error = ScalarIndexDomain::Integer
        .visit_predicate(
            &Predicate::GreaterThan(Value::Int(0)),
            &control,
            &mut |_| {
                visited = true;
                Ok(())
            },
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert!(!visited);
    assert_eq!(control.memory().used(), 0);
}

proptest! {
    #[test]
    fn integer_ranges_match_selected_index_comparisons(key in any::<i64>(), target in any::<f64>().prop_filter("supported predicate", |value| !value.is_nan())) {
        let key = Value::Int(key);
        let index = ColumnValueIndex::build("key", std::iter::once((1, key.clone())));
        for predicate in [Predicate::Equals(Value::Float(target)), Predicate::GreaterThan(Value::Float(target)), Predicate::LessThanOrEqual(Value::Float(target))] {
            prop_assert_eq!(observed(ScalarIndexDomain::Integer, &predicate, &key), !index.scan(&predicate).unwrap().is_empty());
        }
    }

    #[test]
    fn float_ranges_match_selected_index_comparisons(key in any::<f64>(), target in any::<i64>()) {
        let key = Value::Float(key);
        let index = ColumnValueIndex::build("key", std::iter::once((1, key.clone())));
        for predicate in [Predicate::Equals(Value::Int(target)), Predicate::GreaterThan(Value::Int(target)), Predicate::LessThanOrEqual(Value::Int(target))] {
            prop_assert_eq!(observed(ScalarIndexDomain::Float, &predicate, &key), !index.scan(&predicate).unwrap().is_empty());
        }
    }
}
