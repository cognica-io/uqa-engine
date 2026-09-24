//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{DecimalValue, Value};
use std::collections::BTreeSet;

fn decimal(text: &str) -> Value {
    Value::Decimal(DecimalValue::parse(text).unwrap())
}

#[test]
fn numeric_keys_preserve_transitivity_at_binary_decimal_boundaries() {
    let integer = Value::Int(9_223_372_036_854_774_784);
    let float = Value::Float(9_223_372_036_854_774_784_i64 as f64);
    let exact = decimal("9223372036854774784");
    let displayed = decimal("9223372036854775000");
    assert_eq!(integer, float);
    assert_eq!(integer, exact);
    assert_eq!(float, exact);
    assert!(integer < displayed);
    assert!(float < displayed);
    assert_eq!(BTreeSet::from([integer, float, exact, displayed]).len(), 2);
}

#[test]
fn numeric_keys_compare_binary_values_instead_of_their_display_rounding() {
    // Python Decimal.from_float supplies the exact independently converted binary value.
    let exact_tenth = decimal("0.1000000000000000055511151231257827021181583404541015625");
    let float_tenth = Value::Float(0.1);
    assert_eq!(float_tenth, exact_tenth);
    assert!(float_tenth > decimal("0.1"));
    assert!(Value::Float(-0.1) < decimal("-0.1"));
    assert!(Value::Float(f64::from_bits(1)) > decimal("0"));
    assert!(Value::Float(f64::MAX) < decimal("1e309"));
    assert_eq!(Value::Float(-0.0), decimal("0.000"));
    for text in ["NaN", "Infinity", "-Infinity"] {
        let float = text.parse::<f64>().unwrap();
        assert_eq!(Value::Float(float), decimal(text));
    }
}

#[test]
fn numeric_keys_keep_order_and_container_results_independent_of_insertion_order() {
    let values = vec![
        decimal("NaN"),
        Value::Float(f64::NAN),
        decimal("Infinity"),
        Value::Float(f64::INFINITY),
        decimal("-Infinity"),
        Value::Float(f64::NEG_INFINITY),
        decimal("1e309"),
        Value::Float(f64::MAX),
        Value::Int(i64::MIN),
        Value::Int(i64::MAX),
        Value::Int(9_223_372_036_854_774_784),
        Value::Float(9_223_372_036_854_774_784_i64 as f64),
        decimal("9223372036854774784"),
        decimal("9223372036854775000"),
        Value::Int(9_007_199_254_740_993),
        Value::Float(9_007_199_254_740_992.0),
        decimal("9007199254740993"),
        Value::Float(-0.1),
        decimal("-0.1"),
        Value::Int(0),
        Value::Float(-0.0),
        decimal("0.000"),
        Value::Bool(false),
        Value::Float(f64::from_bits(1)),
        Value::Float(0.1),
        decimal("0.1"),
        decimal("0.1000000000000000055511151231257827021181583404541015625"),
        Value::Bool(true),
        Value::Int(1),
        Value::Float(1.0),
        decimal("1.00"),
    ];
    for a in &values {
        for b in &values {
            assert_eq!(a.cmp(b), b.cmp(a).reverse());
            for c in &values {
                if a <= b && b <= c {
                    assert!(a <= c, "non-transitive ordering: {a:?}, {b:?}, {c:?}");
                }
            }
        }
    }
    let expected: BTreeSet<_> = values.iter().cloned().collect();
    for offset in 0..values.len() {
        let reordered: BTreeSet<_> = values[offset..]
            .iter()
            .chain(&values[..offset])
            .rev()
            .cloned()
            .collect();
        assert_eq!(reordered, expected);
    }
}
