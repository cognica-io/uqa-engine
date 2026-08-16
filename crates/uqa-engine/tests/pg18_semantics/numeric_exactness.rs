//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 binary integer casts and arbitrary-precision numeric regressions.

use super::*;

#[test]
fn pg18_integer_bytea_casts_use_network_byte_order() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT encode(((-2)::smallint)::bytea, 'hex')"),
        Value::Str("fffe".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT encode(((-2)::integer)::bytea, 'hex')"),
        Value::Str("fffffffe".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT encode(((-2)::bigint)::bytea, 'hex')"),
        Value::Str("fffffffffffffffe".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT encode(1::bytea, 'hex')"),
        Value::Str("00000001".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT decode('fffe', 'hex')::smallint"),
        Value::Int(-2)
    );
    assert_eq!(
        scalar(&eng, "SELECT decode('fffffffe', 'hex')::integer"),
        Value::Int(-2)
    );
    assert_eq!(
        scalar(&eng, "SELECT decode('ffffffffffffffff', 'hex')::bigint"),
        Value::Int(-1)
    );
    assert_eq!(
        scalar(&eng, "SELECT decode('ff', 'hex')::integer"),
        Value::Int(255)
    );
    assert!(
        scalar_err(&eng, "SELECT decode('0102030405', 'hex')::integer")
            .contains("integer out of range")
    );
}

#[test]
fn incompatible_case_and_array_types_are_rejected_during_binding() {
    let eng = engine();
    for sql in [
        "SELECT pg_typeof(CASE WHEN true THEN NULL::smallint ELSE NULL::text END)",
        "SELECT pg_typeof(ARRAY[NULL::smallint, NULL::text])",
    ] {
        assert!(eng.sql(sql, &[]).is_err(), "{sql}");
    }
}

#[test]
fn numeric_is_arbitrary_precision_and_orders_postgresql_special_values() {
    let eng = engine();
    assert_eq!(
        scalar(
            &eng,
            "SELECT 123456789012345678901234567890.123456789::numeric"
        ),
        dec("123456789012345678901234567890.123456789")
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT array_sort(ARRAY['NaN'::numeric, 1::numeric, '-Infinity'::numeric, 'Infinity'::numeric, -1::numeric, NULL])"
        ),
        array(vec![
            dec("-Infinity"),
            dec("-1"),
            dec("1"),
            dec("Infinity"),
            dec("NaN"),
            Value::Null,
        ])
    );
}

#[test]
fn jsonb_keeps_arbitrary_precision_numeric_values_exact() {
    let eng = engine();
    let number = "123456789012345678901234567890.123456789";
    assert_eq!(
        scalar(&eng, &format!("SELECT '{number}'::jsonb")),
        Value::JsonB(number.into())
    );
    assert_eq!(
        scalar(
            &eng,
            &format!("SELECT jsonb_build_array({number}::numeric)")
        ),
        Value::JsonB(format!("[{number}]"))
    );
    assert_eq!(
        scalar(
            &eng,
            &format!("SELECT jsonb_extract_path('[{number}]'::jsonb, '0')")
        ),
        Value::JsonB(number.into())
    );
}
