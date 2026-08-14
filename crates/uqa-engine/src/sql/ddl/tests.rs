//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::coerce_json_value;
use uqa_core::Value;

#[test]
fn json_coercion_rejects_invalid_json_strings() {
    assert!(coerce_json_value(Value::Str("{invalid".into()), true).is_err());
    assert!(matches!(
        coerce_json_value(Value::Str("{\"ok\":true}".into()), true).unwrap(),
        Value::JsonB(_)
    ));
}
