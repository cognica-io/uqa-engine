//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{coerce_json_value, float_to_integer};
use uqa_core::Value;

#[test]
fn integer_coercion_rejects_non_finite_and_out_of_range_floats() {
    assert_eq!(float_to_integer(12.9).unwrap(), 12);
    for value in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        9_223_372_036_854_775_808.0,
        -9_223_372_036_854_777_856.0,
    ] {
        assert!(float_to_integer(value).is_err(), "accepted {value:?}");
    }
}

#[test]
fn json_coercion_rejects_invalid_json_strings() {
    assert!(coerce_json_value(Value::Str("{invalid".into())).is_err());
    assert!(matches!(
        coerce_json_value(Value::Str("{\"ok\":true}".into())).unwrap(),
        Value::Map(_)
    ));
}
