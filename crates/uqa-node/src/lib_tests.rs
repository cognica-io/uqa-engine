use super::*;
use crate::value::value_from_js_number;

#[test]
fn unsafe_integer_numbers_require_bigint() {
    let error = value_from_js_number((MAX_SAFE_INTEGER + 1) as f64)
        .expect_err("unsafe integer-valued Numbers must not become approximate floats");
    assert!(error.to_string().contains("pass a BigInt"));
    assert_eq!(
        value_from_js_number(MAX_SAFE_INTEGER as f64).unwrap(),
        Value::Int(MAX_SAFE_INTEGER)
    );
}

#[test]
fn fractional_and_non_finite_numbers_remain_floats() {
    assert_eq!(value_from_js_number(1.5).unwrap(), Value::Float(1.5));
    assert!(matches!(
        value_from_js_number(f64::NAN).unwrap(),
        Value::Float(value) if value.is_nan()
    ));
}
