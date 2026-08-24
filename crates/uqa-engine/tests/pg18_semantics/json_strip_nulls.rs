//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `json_strip_nulls` and `jsonb_strip_nulls` parity.

use super::*;
use uqa_engine::SQLParam;

#[test]
fn pg18_json_null_stripping_preserves_each_storage_contract() {
    let eng = engine();
    let input = r#"{"z":1,"a":null,"z":2,"s":"\u0061","n":1.2300e+02,"x":[null,{"d":null,"k":3}]}"#;
    assert_eq!(
        scalar(&eng, &format!("SELECT json_strip_nulls('{input}'::json)")),
        Value::Json(r#"{"z":1,"z":2,"s":"a","n":1.2300e+02,"x":[null,{"k":3}]}"#.into())
    );
    assert_eq!(
        scalar(
            &eng,
            &format!("SELECT json_strip_nulls('{input}'::json, true)")
        ),
        Value::Json(r#"{"z":1,"z":2,"s":"a","n":1.2300e+02,"x":[{"k":3}]}"#.into())
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT jsonb_strip_nulls('{\"z\":1,\"a\":null,\"z\":2,\"x\":[null,{\"d\":null,\"k\":3}]}'::jsonb, true)"
        ),
        Value::JsonB(r#"{"x": [{"k": 3}], "z": 2}"#.into())
    );
    for (sql, expected) in [
        (
            "SELECT json_strip_nulls('null'::json)",
            Value::Json("null".into()),
        ),
        (
            "SELECT json_strip_nulls('[null]'::json)",
            Value::Json("[null]".into()),
        ),
        (
            "SELECT json_strip_nulls('[null]'::json, true)",
            Value::Json("[]".into()),
        ),
        (
            "SELECT jsonb_strip_nulls('[null]'::jsonb, false)",
            Value::JsonB("[null]".into()),
        ),
    ] {
        assert_eq!(scalar(&eng, sql), expected, "{sql}");
    }
}

#[test]
fn pg18_json_null_stripping_binds_defaults_names_types_and_parameters() {
    let eng = engine();
    for (sql, expected) in [
        (
            "SELECT json_strip_nulls(target => '{\"a\":null,\"x\":[null]}'::json)",
            Value::Json(r#"{"x":[null]}"#.into()),
        ),
        (
            "SELECT json_strip_nulls(strip_in_arrays => true, target => '{\"a\":null,\"x\":[null]}'::json)",
            Value::Json(r#"{"x":[]}"#.into()),
        ),
        (
            "SELECT jsonb_strip_nulls(strip_in_arrays => false, target => '{\"a\":null,\"x\":[null]}'::jsonb)",
            Value::JsonB(r#"{"x": [null]}"#.into()),
        ),
        (
            "SELECT json_strip_nulls((SELECT '{\"a\":null,\"b\":1}'::json))",
            Value::Json(r#"{"b":1}"#.into()),
        ),
    ] {
        assert_eq!(scalar(&eng, sql), expected, "{sql}");
    }
    for (sql, expected) in [
        ("SELECT pg_typeof(json_strip_nulls(NULL))", "json"),
        ("SELECT pg_typeof(json_strip_nulls('{}'))", "json"),
        ("SELECT pg_typeof(jsonb_strip_nulls(NULL))", "jsonb"),
        ("SELECT pg_typeof(jsonb_strip_nulls('{}'))", "jsonb"),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Str(expected.into()), "{sql}");
    }
    assert_eq!(scalar(&eng, "SELECT json_strip_nulls(NULL)"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT json_strip_nulls('{}'::json, NULL)"),
        Value::Null
    );

    let positional = eng
        .sql(
            "SELECT json_strip_nulls($1, $2) AS value, pg_typeof(json_strip_nulls($1, $2)) AS ty",
            &[
                SQLParam::Scalar(Value::Str(r#"{"a":null,"x":[null]}"#.into())),
                SQLParam::Scalar(Value::Str("true".into())),
            ],
        )
        .unwrap();
    assert_eq!(
        positional.rows[0]["value"],
        Value::Json(r#"{"x":[]}"#.into())
    );
    assert_eq!(positional.rows[0]["ty"], Value::Str("json".into()));
    let named = eng
        .sql(
            "SELECT jsonb_strip_nulls(strip_in_arrays => $1, target => $2) AS value, pg_typeof(jsonb_strip_nulls(strip_in_arrays => $1, target => $2)) AS ty",
            &[
                SQLParam::Scalar(Value::Bool(true)),
                SQLParam::Scalar(Value::Str(r#"{"a":null,"x":[null]}"#.into())),
            ],
        )
        .unwrap();
    assert_eq!(named.rows[0]["value"], Value::JsonB(r#"{"x": []}"#.into()));
    assert_eq!(named.rows[0]["ty"], Value::Str("jsonb".into()));
}

#[test]
fn pg18_json_null_stripping_reports_declared_signature_errors() {
    let eng = engine();
    for sql in [
        "SELECT json_strip_nulls()",
        "SELECT json_strip_nulls('{}'::json, false, true)",
        "SELECT json_strip_nulls('{}'::text)",
        "SELECT json_strip_nulls('{}'::jsonb)",
        "SELECT jsonb_strip_nulls('{}'::json)",
        "SELECT json_strip_nulls('{}'::json, 1)",
        "SELECT json_strip_nulls('{}'::json, 'false'::text)",
        "SELECT json_strip_nulls(bad => '{}'::json)",
        "SELECT pg_catalog.jsonb_strip_nulls('{}'::text)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
    let malformed = eng
        .sql("SELECT json_strip_nulls('{\"a\":}' )", &[])
        .unwrap_err();
    assert_eq!(malformed.sqlstate(), Some("22P02"));
    for sql in [
        "SELECT json_strip_nulls(target => '{}'::json, false)",
        "SELECT json_strip_nulls(target => '{}'::json, target => '{}'::json)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42601"), "{sql}: {error}");
    }
}

#[test]
fn pg18_json_null_stripping_ranks_defaults_and_user_overloads_by_search_path() {
    let eng = engine();
    for sql in [
        "CREATE SCHEMA json_strip_overload",
        "CREATE FUNCTION json_strip_overload.json_strip_nulls(target JSON) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-json-one'''",
        "CREATE FUNCTION json_strip_overload.json_strip_nulls(target JSON, strip_in_arrays BOOLEAN) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-json-two'''",
        "CREATE FUNCTION json_strip_overload.json_strip_nulls(target TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-text'''",
        "SET search_path = json_strip_overload, public",
    ] {
        eng.sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    for (sql, expected) in [
        (
            "SELECT json_strip_nulls('{}'::json)::text",
            Value::Str("{}".into()),
        ),
        (
            "SELECT json_strip_nulls('{}'::json, true)::text",
            Value::Str("{}".into()),
        ),
        (
            "SELECT json_strip_nulls('{}'::text)",
            Value::Str("user-text".into()),
        ),
        (
            "SELECT json_strip_nulls('{}')",
            Value::Str("user-text".into()),
        ),
        (
            "SELECT pg_catalog.json_strip_nulls('{}'::json)::text",
            Value::Str("{}".into()),
        ),
        (
            "SELECT json_strip_overload.json_strip_nulls('{}'::json)",
            Value::Str("user-json-one".into()),
        ),
    ] {
        assert_eq!(scalar(&eng, sql), expected, "{sql}");
    }

    eng.sql(
        "SET search_path = json_strip_overload, pg_catalog, public",
        &[],
    )
    .unwrap();
    for (sql, expected) in [
        ("SELECT json_strip_nulls('{}'::json)", "user-json-one"),
        ("SELECT json_strip_nulls('{}'::json, true)", "user-json-two"),
        ("SELECT json_strip_nulls('{}')", "user-text"),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Str(expected.into()), "{sql}");
    }

    let ambiguous = engine();
    for sql in [
        "CREATE SCHEMA json_strip_ambiguous",
        "CREATE FUNCTION json_strip_ambiguous.json_strip_nulls(target JSONB) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-jsonb'''",
        "SET search_path = json_strip_ambiguous, public",
    ] {
        ambiguous
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    let error = ambiguous
        .sql("SELECT json_strip_nulls('{}')", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42725"));
}
