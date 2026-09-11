//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{ArrayValue, DecimalValue};

#[test]
fn value_to_usize_rejects_non_finite_fractional_and_out_of_range_floats() {
    assert_eq!(value_to_usize(&Value::Float(42.0)).unwrap(), 42);
    for value in [f64::NAN, f64::INFINITY, -1.0, 1.5] {
        assert!(value_to_usize(&Value::Float(value)).is_err());
    }
    let exponent = i32::try_from(usize::BITS).unwrap();
    assert!(value_to_usize(&Value::Float(2.0_f64.powi(exponent))).is_err());
}

#[test]
fn training_features_preserve_numeric_values_and_array_storage_order() {
    let values = vec![
        Value::Int(-2),
        Value::Float(3.5),
        Value::Decimal(DecimalValue::parse("4.2500").unwrap()),
    ];
    let expected = vec![-2.0, 3.5, 4.25];
    assert_eq!(
        value_to_f64_vec(&Value::List(values.clone())).unwrap(),
        expected
    );
    let array = ArrayValue::with_lower_bounds(values, vec![-5]).unwrap();
    assert_eq!(value_to_f64_vec(&Value::Array(array)).unwrap(), expected);
    for empty in [
        Value::List(Vec::new()),
        Value::Array(ArrayValue::try_new(Vec::new()).unwrap()),
    ] {
        assert!(value_to_f64_vec(&empty).unwrap().is_empty());
    }
}

#[test]
fn training_feature_diagnostics_distinguish_shape_elements_and_decimal_range() {
    assert_eq!(
        value_to_f64_vec(&Value::Null).unwrap_err(),
        "expected feature array, got Null"
    );
    assert_eq!(
        value_to_f64_vec(&Value::List(vec![Value::Null])).unwrap_err(),
        "expected numeric feature, got Null"
    );
    let matrix = ArrayValue::try_new(vec![Value::List(vec![Value::Int(1)])]).unwrap();
    assert_eq!(
        value_to_f64_vec(&Value::Array(matrix)).unwrap_err(),
        "expected one-dimensional feature array, got 2 dimensions"
    );
    let huge = Value::Decimal(DecimalValue::parse("1e1000").unwrap());
    assert_eq!(
        value_to_f64_vec(&Value::List(vec![huge])).unwrap_err(),
        "decimal feature is outside f64 range"
    );
}

#[test]
fn training_labels_accept_zero_and_reject_non_integer_value_kinds() {
    for value in [Value::Int(0), Value::Float(-0.0)] {
        assert_eq!(value_to_usize(&value).unwrap(), 0);
    }
    assert_eq!(
        value_to_usize(&Value::Int(-1)).unwrap_err(),
        "expected non-negative integer label, got Int(-1)"
    );
    for value in [
        Value::Null,
        Value::Bool(true),
        Value::Str("1".into()),
        Value::Decimal(DecimalValue::parse("1").unwrap()),
    ] {
        assert_eq!(
            value_to_usize(&value).unwrap_err(),
            format!("expected non-negative integer label, got {value:?}")
        );
    }
}
