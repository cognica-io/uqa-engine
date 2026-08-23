//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column parity for `reverse(text|bytea)` overload binding.

use super::*;

#[test]
fn generated_columns_bind_reverse_text_and_bytea_overloads() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_reverse (
                 source TEXT,
                 bytes BYTEA,
                 reversed TEXT GENERATED ALWAYS AS (reverse(source)) STORED,
                 reversed_bytes BYTEA GENERATED ALWAYS AS (reverse(bytes)) STORED,
                 null_type TEXT GENERATED ALWAYS AS (reverse(NULL)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_reverse(source, bytes) VALUES ('abc', decode('00ff10', 'hex'))",
            &[],
        )
        .unwrap();
    let row = engine
        .sql(
            "SELECT reversed, reversed_bytes, null_type FROM generated_reverse",
            &[],
        )
        .unwrap();
    assert_eq!(row.rows[0]["reversed"], Value::Str("cba".into()));
    assert_eq!(
        row.rows[0]["reversed_bytes"],
        Value::Bytes(vec![0x10, 0xff, 0x00])
    );
    assert_eq!(row.rows[0]["null_type"], Value::Null);

    for sql in [
        "CREATE TABLE invalid_reverse_integer (source INTEGER, value TEXT GENERATED ALWAYS AS (reverse(source)) STORED)",
        "CREATE TABLE invalid_reverse_arity (source TEXT, value TEXT GENERATED ALWAYS AS (reverse(source, source)) STORED)",
        "CREATE TABLE invalid_reverse_named (source TEXT, value TEXT GENERATED ALWAYS AS (reverse(string => source)) STORED)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
}

#[test]
fn generated_columns_rank_reverse_user_overloads_by_search_path() {
    let engine = Engine::new();
    create_reverse_overload_fixture(&engine);
    let default_path = engine
        .sql(
            "SELECT builtin_value, varying_value, integer_value FROM generated_reverse_default",
            &[],
        )
        .unwrap();
    assert_eq!(
        default_path.rows[0]["builtin_value"],
        Value::Str("cba".into())
    );
    assert_eq!(
        default_path.rows[0]["varying_value"],
        Value::Str("user-varchar".into())
    );
    assert_eq!(
        default_path.rows[0]["integer_value"],
        Value::Str("user-int".into())
    );
    let stored_expression = engine
        .sql(
            "SELECT generation_expression FROM information_schema.columns \
             WHERE table_schema = 'reverse_generated' \
               AND table_name = 'generated_reverse_default' \
               AND column_name = 'builtin_value'",
            &[],
        )
        .unwrap();
    assert_eq!(
        stored_expression.rows[0]["generation_expression"],
        Value::Str("reverse(source)".into())
    );

    engine
        .sql(
            "SET search_path = reverse_generated, pg_catalog, public",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_reverse_default(source, varying, number) VALUES ('def', 'def', 2)",
            &[],
        )
        .unwrap();
    let stable_builtin = engine
        .sql(
            "SELECT builtin_value, varying_value, integer_value FROM generated_reverse_default WHERE source = 'def'",
            &[],
        )
        .unwrap();
    assert_eq!(
        stable_builtin.rows[0]["builtin_value"],
        Value::Str("fed".into())
    );
    assert_eq!(
        stable_builtin.rows[0]["varying_value"],
        Value::Str("user-varchar".into())
    );
    assert_eq!(
        stable_builtin.rows[0]["integer_value"],
        Value::Str("user-int".into())
    );
    engine
        .sql(
            "CREATE TABLE generated_reverse_explicit (
                 source TEXT,
                 value TEXT GENERATED ALWAYS AS (reverse(source)) STORED
             )",
            &[],
        )
        .unwrap();
    engine.sql("SET search_path = public", &[]).unwrap();
    engine
        .sql(
            "INSERT INTO reverse_generated.generated_reverse_explicit(source) VALUES ('abc')",
            &[],
        )
        .unwrap();
    let explicit_path = engine
        .sql(
            "SELECT value FROM reverse_generated.generated_reverse_explicit",
            &[],
        )
        .unwrap();
    assert_eq!(
        explicit_path.rows[0]["value"],
        Value::Str("user-text".into())
    );
}

fn create_reverse_overload_fixture(engine: &Engine) {
    for sql in [
        "CREATE SCHEMA reverse_generated",
        "CREATE FUNCTION reverse_generated.reverse(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-text'''",
        "CREATE FUNCTION reverse_generated.reverse(value VARCHAR) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-varchar'''",
        "CREATE FUNCTION reverse_generated.reverse(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-int'''",
        "SET search_path = reverse_generated, public",
        "CREATE TABLE generated_reverse_default (
             source TEXT,
             varying VARCHAR,
             number INTEGER,
             builtin_value TEXT GENERATED ALWAYS AS (reverse(source)) STORED,
             varying_value TEXT GENERATED ALWAYS AS (reverse(varying)) STORED,
             integer_value TEXT GENERATED ALWAYS AS (reverse(number)) STORED
         )",
        "INSERT INTO generated_reverse_default(source, varying, number) VALUES ('abc', 'abc', 1)",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn generated_reverse_builtin_binding_survives_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("reverse-binding.uqa");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE SCHEMA reverse_persistent",
            "CREATE FUNCTION reverse_persistent.reverse(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-text'''",
            "SET search_path = reverse_persistent, public",
            "CREATE TABLE reverse_persistent.generated_values (
                 source TEXT,
                 value TEXT GENERATED ALWAYS AS (reverse(source)) STORED
             )",
        ] {
            engine.sql(sql, &[]).unwrap();
        }
    }
    let engine = Engine::open(&database).unwrap();
    engine
        .sql(
            "SET search_path = reverse_persistent, pg_catalog, public",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO reverse_persistent.generated_values(source) VALUES ('abc')",
            &[],
        )
        .unwrap();
    let result = engine
        .sql("SELECT value FROM reverse_persistent.generated_values", &[])
        .unwrap();
    assert_eq!(result.rows[0]["value"], Value::Str("cba".into()));
}
