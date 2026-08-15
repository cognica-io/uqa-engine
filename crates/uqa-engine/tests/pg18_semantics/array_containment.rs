//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{engine, scalar, scalar_err};
use uqa_core::Value;

#[test]
fn array_containment_uses_postgresql_element_semantics() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT ARRAY[1,2,3] @> ARRAY[2]"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT ARRAY[2] <@ ARRAY[1,2,3]"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT ARRAY[1,2] @> ARRAY[2,2]"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT ARRAY[[1,2],[3,4]] @> ARRAY[2,4]"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT '[0:1]={1,2}'::int[] @> ARRAY[2]"),
        Value::Bool(true)
    );
    assert_eq!(scalar(&eng, "SELECT ARRAY[1] @> '{1}'"), Value::Bool(true));
    assert_eq!(
        scalar(&eng, "SELECT ARRAY[NULL::int] @> ARRAY[NULL::int]"),
        Value::Bool(false)
    );
}

#[test]
fn containment_operator_is_strict_and_type_resolved() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT NULL::int[] @> ARRAY[1]"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT ARRAY[]::int[] @> ARRAY[]::int[]"),
        Value::Bool(true)
    );
    assert!(scalar_err(&eng, "SELECT NULL @> NULL").contains("operator is not unique"));
    assert!(
        scalar_err(&eng, "SELECT ARRAY[1]::int[] @> ARRAY[1]::bigint[]")
            .contains("operator does not exist")
    );
    assert!(scalar_err(&eng, "SELECT '{}'::json @> '{}'::json").contains("operator does not exist"));
}

#[test]
fn jsonb_containment_still_uses_jsonb_semantics() {
    let eng = engine();
    assert_eq!(
        scalar(
            &eng,
            "SELECT '{\"a\":1,\"b\":[1,2]}'::jsonb @> '{\"b\":[2]}'"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT '{\"a\":1}'::jsonb <@ '{\"a\":1,\"b\":2}'::jsonb"
        ),
        Value::Bool(true)
    );
}
