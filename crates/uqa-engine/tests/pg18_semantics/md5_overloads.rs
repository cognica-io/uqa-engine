//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `md5(text|bytea)` overload parity.

use super::*;
use uqa_engine::SQLParam;

#[test]
fn pg18_md5_preserves_text_and_bytea_overloads() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT md5('abc')"),
        Value::Str("900150983cd24fb0d6963f7d28e17f72".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT md5(decode('00ff10', 'hex'))"),
        Value::Str("481e4551ec039aada760901cf52b1917".into())
    );
    for (sql, expected) in [
        ("SELECT pg_typeof(md5(NULL))", "text"),
        ("SELECT pg_typeof(md5(NULL::bytea))", "text"),
        ("SELECT pg_typeof(md5('abc'::varchar))", "text"),
        ("SELECT pg_typeof(md5('abc'::bpchar))", "text"),
        ("SELECT pg_typeof(md5('abc'::name))", "text"),
        ("SELECT pg_typeof(md5('a'::\"char\"))", "text"),
        (
            "SELECT pg_typeof(md5((SELECT decode('00ff10', 'hex'))))",
            "text",
        ),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Str(expected.into()), "{sql}");
    }
    assert_eq!(scalar(&eng, "SELECT md5(NULL)"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT md5(NULL::bytea)"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT pg_catalog.md5('abc')"),
        Value::Str("900150983cd24fb0d6963f7d28e17f72".into())
    );

    for (parameter, expected_hash) in [
        (
            Value::Str("abc".into()),
            Value::Str("900150983cd24fb0d6963f7d28e17f72".into()),
        ),
        (
            Value::Bytes(vec![0x00, 0xff, 0x10]),
            Value::Str("481e4551ec039aada760901cf52b1917".into()),
        ),
    ] {
        let result = eng
            .sql(
                "SELECT md5($1) AS value, pg_typeof(md5($1)) AS ty",
                &[SQLParam::Scalar(parameter)],
            )
            .unwrap();
        assert_eq!(result.rows[0]["value"], expected_hash);
        assert_eq!(result.rows[0]["ty"], Value::Str("text".into()));
    }
    let unknown = eng
        .sql(
            "SELECT md5($1) AS value, pg_typeof(md5($1)) AS ty",
            &[SQLParam::Scalar(Value::Null)],
        )
        .unwrap();
    assert_eq!(unknown.rows[0]["value"], Value::Null);
    assert_eq!(unknown.rows[0]["ty"], Value::Str("text".into()));
}

#[test]
fn pg18_md5_rejects_missing_overloads() {
    let eng = engine();
    for (sql, signature) in [
        ("SELECT md5()", "md5()"),
        ("SELECT md5(1)", "md5(integer)"),
        ("SELECT md5('abc', 'def')", "md5(unknown, unknown)"),
        ("SELECT md5(value => 'abc')", "md5(value => unknown)"),
        ("SELECT md5(ARRAY[1])", "md5(integer[])"),
        ("SELECT pg_catalog.md5(1)", "pg_catalog.md5(integer)"),
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
fn pg18_md5_ranks_user_overloads_and_pg_catalog_search_order() {
    let eng = engine();
    for sql in [
        "CREATE SCHEMA md5_overload",
        "CREATE FUNCTION md5_overload.md5(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-text'''",
        "CREATE FUNCTION md5_overload.md5(value VARCHAR) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-varchar'''",
        "CREATE FUNCTION md5_overload.md5(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-int'''",
        "SET search_path = md5_overload, public",
    ] {
        eng.sql(sql, &[]).unwrap();
    }
    for (sql, expected) in [
        (
            "SELECT md5('abc'::text)",
            "900150983cd24fb0d6963f7d28e17f72",
        ),
        ("SELECT md5('abc'::varchar)", "user-varchar"),
        ("SELECT md5('abc')", "900150983cd24fb0d6963f7d28e17f72"),
        ("SELECT md5(1)", "user-int"),
        (
            "SELECT pg_catalog.md5('abc')",
            "900150983cd24fb0d6963f7d28e17f72",
        ),
        ("SELECT md5_overload.md5('abc')", "user-text"),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Str(expected.into()), "{sql}");
    }

    eng.sql("SET search_path = md5_overload, pg_catalog, public", &[])
        .unwrap();
    for (sql, expected) in [
        ("SELECT md5('abc'::text)", "user-text"),
        ("SELECT md5('abc'::varchar)", "user-varchar"),
        ("SELECT md5('abc')", "user-text"),
        ("SELECT md5(1)", "user-int"),
        (
            "SELECT md5(decode('00ff10', 'hex'))",
            "481e4551ec039aada760901cf52b1917",
        ),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Str(expected.into()), "{sql}");
    }
}
