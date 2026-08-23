//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `reverse(text|bytea)` overload parity.

use super::*;
use uqa_engine::SQLParam;

#[test]
fn pg18_reverse_preserves_text_and_bytea_overloads() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT reverse('abc')"),
        Value::Str("cba".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT reverse(decode('00ff10', 'hex'))"),
        Value::Bytes(vec![0x10, 0xff, 0x00])
    );
    for (sql, expected) in [
        ("SELECT pg_typeof(reverse(NULL))", "text"),
        ("SELECT pg_typeof(reverse(NULL::bytea))", "bytea"),
        ("SELECT pg_typeof(reverse('abc'::varchar))", "text"),
        ("SELECT pg_typeof(reverse('abc'::bpchar))", "text"),
        ("SELECT pg_typeof(reverse('abc'::name))", "text"),
        ("SELECT pg_typeof(reverse('a'::\"char\"))", "text"),
        (
            "SELECT pg_typeof(reverse((SELECT decode('00ff10', 'hex'))))",
            "bytea",
        ),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Str(expected.into()), "{sql}");
    }
    assert_eq!(scalar(&eng, "SELECT reverse(NULL)"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT reverse(NULL::bytea)"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT pg_catalog.reverse('abc')"),
        Value::Str("cba".into())
    );

    let text_param = eng
        .sql(
            "SELECT reverse($1) AS value, pg_typeof(reverse($1)) AS ty",
            &[SQLParam::Scalar(Value::Str("abc".into()))],
        )
        .unwrap();
    assert_eq!(text_param.rows[0]["value"], Value::Str("cba".into()));
    assert_eq!(text_param.rows[0]["ty"], Value::Str("text".into()));
    let unknown_param = eng
        .sql(
            "SELECT reverse($1) AS value, pg_typeof(reverse($1)) AS ty",
            &[SQLParam::Scalar(Value::Null)],
        )
        .unwrap();
    assert_eq!(unknown_param.rows[0]["value"], Value::Null);
    assert_eq!(unknown_param.rows[0]["ty"], Value::Str("text".into()));
}

#[test]
fn pg18_reverse_rejects_missing_overloads() {
    let eng = engine();
    for (sql, signature) in [
        ("SELECT reverse()", "reverse()"),
        ("SELECT reverse(1)", "reverse(integer)"),
        ("SELECT reverse('abc', 'def')", "reverse(unknown, unknown)"),
        (
            "SELECT reverse(string => 'abc')",
            "reverse(string => unknown)",
        ),
        ("SELECT reverse(ARRAY[1])", "reverse(integer[])"),
        (
            "SELECT pg_catalog.reverse(1)",
            "pg_catalog.reverse(integer)",
        ),
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
        assert_eq!(
            error.to_string(),
            format!("function {signature} does not exist"),
            "{sql}"
        );
    }
}

#[test]
fn pg18_reverse_ranks_user_overloads_and_pg_catalog_search_order() {
    let eng = engine();
    for sql in [
        "CREATE SCHEMA reverse_overload",
        "CREATE FUNCTION reverse_overload.reverse(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-text'''",
        "CREATE FUNCTION reverse_overload.reverse(value VARCHAR) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-varchar'''",
        "CREATE FUNCTION reverse_overload.reverse(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-int'''",
        "SET search_path = reverse_overload, public",
    ] {
        eng.sql(sql, &[]).unwrap();
    }
    for (sql, expected) in [
        ("SELECT reverse('abc'::text)", "cba"),
        ("SELECT reverse('abc'::varchar)", "user-varchar"),
        ("SELECT reverse('abc')", "cba"),
        ("SELECT reverse(1)", "user-int"),
        ("SELECT pg_catalog.reverse('abc')", "cba"),
        ("SELECT reverse_overload.reverse('abc')", "user-text"),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Str(expected.into()), "{sql}");
    }

    eng.sql(
        "SET search_path = reverse_overload, pg_catalog, public",
        &[],
    )
    .unwrap();
    for (sql, expected) in [
        ("SELECT reverse('abc'::text)", "user-text"),
        ("SELECT reverse('abc'::varchar)", "user-varchar"),
        ("SELECT reverse('abc')", "user-text"),
        ("SELECT reverse(1)", "user-int"),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Str(expected.into()), "{sql}");
    }
}
