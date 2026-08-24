//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column parity for JSON null-stripping bindings.

use super::*;

#[test]
fn generated_columns_bind_json_null_stripping_signatures_and_defaults() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_json_strip_values (
                 source_json JSON,
                 source_jsonb JSONB,
                 default_json JSON GENERATED ALWAYS AS (json_strip_nulls(source_json)) STORED,
                 arrays_json JSON GENERATED ALWAYS AS (json_strip_nulls(strip_in_arrays => true, target => source_json)) STORED,
                 default_jsonb JSONB GENERATED ALWAYS AS (jsonb_strip_nulls(source_jsonb)) STORED,
                 arrays_jsonb JSONB GENERATED ALWAYS AS (jsonb_strip_nulls(strip_in_arrays => true, target => source_jsonb)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_json_strip_values(source_json, source_jsonb) VALUES ('{\"z\":1,\"a\":null,\"z\":2,\"x\":[null]}', '{\"z\":1,\"a\":null,\"z\":2,\"x\":[null]}')",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT default_json, arrays_json, default_jsonb, arrays_jsonb FROM generated_json_strip_values",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows[0]["default_json"],
        Value::Json(r#"{"z":1,"z":2,"x":[null]}"#.into())
    );
    assert_eq!(
        result.rows[0]["arrays_json"],
        Value::Json(r#"{"z":1,"z":2,"x":[]}"#.into())
    );
    assert_eq!(
        result.rows[0]["default_jsonb"],
        Value::JsonB(r#"{"x": [null], "z": 2}"#.into())
    );
    assert_eq!(
        result.rows[0]["arrays_jsonb"],
        Value::JsonB(r#"{"x": [], "z": 2}"#.into())
    );

    for sql in [
        "CREATE TABLE invalid_json_strip_text (source TEXT, value JSON GENERATED ALWAYS AS (json_strip_nulls(source)) STORED)",
        "CREATE TABLE invalid_json_strip_cross (source JSONB, value JSON GENERATED ALWAYS AS (json_strip_nulls(source)) STORED)",
        "CREATE TABLE invalid_json_strip_flag (source JSON, flag INTEGER, value JSON GENERATED ALWAYS AS (json_strip_nulls(source, flag)) STORED)",
        "CREATE TABLE invalid_json_strip_arity (source JSON, value JSON GENERATED ALWAYS AS (json_strip_nulls(source, false, true)) STORED)",
        "CREATE TABLE invalid_json_strip_named (source JSON, value JSON GENERATED ALWAYS AS (json_strip_nulls(bad => source)) STORED)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
}

#[test]
fn generated_json_strip_bindings_preserve_user_selection_across_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("json-strip-binding.uqa");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE SCHEMA json_strip_generated",
            "CREATE FUNCTION json_strip_generated.json_strip_nulls(target JSON) RETURNS JSON LANGUAGE SQL IMMUTABLE AS 'SELECT ''{\"user\":1}''::json'",
            "SET search_path = json_strip_generated, pg_catalog, public",
            "CREATE TABLE json_strip_generated.generated_values (
                 source JSON,
                 user_value JSON GENERATED ALWAYS AS (json_strip_nulls(source)) STORED,
                 builtin_value JSON GENERATED ALWAYS AS (pg_catalog.json_strip_nulls(source)) STORED
             )",
        ] {
            engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
    }
    let engine = Engine::open(&database).unwrap();
    engine
        .sql("SET search_path = pg_catalog, public", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO json_strip_generated.generated_values(source) VALUES ('{\"a\":null,\"b\":2}')",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT user_value, builtin_value FROM json_strip_generated.generated_values",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows[0]["user_value"],
        Value::Json(r#"{"user":1}"#.into())
    );
    assert_eq!(
        result.rows[0]["builtin_value"],
        Value::Json(r#"{"b":2}"#.into())
    );
}
