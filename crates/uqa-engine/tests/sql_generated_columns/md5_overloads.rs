//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column parity for `md5(text|bytea)` overload binding.

use super::*;

#[test]
fn generated_columns_bind_md5_text_and_bytea_overloads() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_md5 (
                 source TEXT,
                 bytes BYTEA,
                 text_hash TEXT GENERATED ALWAYS AS (md5(source)) STORED,
                 bytea_hash TEXT GENERATED ALWAYS AS (md5(bytes)) STORED,
                 null_hash TEXT GENERATED ALWAYS AS (md5(NULL)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_md5(source, bytes) VALUES ('abc', decode('00ff10', 'hex'))",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT text_hash, bytea_hash, null_hash FROM generated_md5",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows[0]["text_hash"],
        Value::Str("900150983cd24fb0d6963f7d28e17f72".into())
    );
    assert_eq!(
        result.rows[0]["bytea_hash"],
        Value::Str("481e4551ec039aada760901cf52b1917".into())
    );
    assert_eq!(result.rows[0]["null_hash"], Value::Null);

    for sql in [
        "CREATE TABLE invalid_md5_integer (source INTEGER, value TEXT GENERATED ALWAYS AS (md5(source)) STORED)",
        "CREATE TABLE invalid_md5_arity (source TEXT, value TEXT GENERATED ALWAYS AS (md5(source, source)) STORED)",
        "CREATE TABLE invalid_md5_named (source TEXT, value TEXT GENERATED ALWAYS AS (md5(value => source)) STORED)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
}

#[test]
fn generated_columns_rank_md5_user_overloads_by_search_path() {
    let engine = Engine::new();
    create_md5_overload_fixture(&engine);
    let default_path = engine
        .sql(
            "SELECT builtin_value, varying_value, integer_value FROM generated_md5_default",
            &[],
        )
        .unwrap();
    assert_eq!(
        default_path.rows[0]["builtin_value"],
        Value::Str("900150983cd24fb0d6963f7d28e17f72".into())
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
             WHERE table_schema = 'md5_generated' \
               AND table_name = 'generated_md5_default' \
               AND column_name = 'builtin_value'",
            &[],
        )
        .unwrap();
    assert_eq!(
        stored_expression.rows[0]["generation_expression"],
        Value::Str("md5(source)".into())
    );

    engine
        .sql("SET search_path = md5_generated, pg_catalog, public", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_md5_default(source, varying, number) VALUES ('def', 'def', 2)",
            &[],
        )
        .unwrap();
    let stable_builtin = engine
        .sql(
            "SELECT builtin_value, varying_value, integer_value FROM generated_md5_default WHERE source = 'def'",
            &[],
        )
        .unwrap();
    assert_eq!(
        stable_builtin.rows[0]["builtin_value"],
        Value::Str("4ed9407630eb1000c0f6b63842defa7d".into())
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
            "CREATE TABLE generated_md5_explicit (
                 source TEXT,
                 value TEXT GENERATED ALWAYS AS (md5(source)) STORED
             )",
            &[],
        )
        .unwrap();
    engine.sql("SET search_path = public", &[]).unwrap();
    engine
        .sql(
            "INSERT INTO md5_generated.generated_md5_explicit(source) VALUES ('abc')",
            &[],
        )
        .unwrap();
    let explicit_path = engine
        .sql(
            "SELECT value FROM md5_generated.generated_md5_explicit",
            &[],
        )
        .unwrap();
    assert_eq!(
        explicit_path.rows[0]["value"],
        Value::Str("user-text".into())
    );
}

fn create_md5_overload_fixture(engine: &Engine) {
    for sql in [
        "CREATE SCHEMA md5_generated",
        "CREATE FUNCTION md5_generated.md5(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-text'''",
        "CREATE FUNCTION md5_generated.md5(value VARCHAR) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-varchar'''",
        "CREATE FUNCTION md5_generated.md5(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-int'''",
        "SET search_path = md5_generated, public",
        "CREATE TABLE generated_md5_default (
             source TEXT,
             varying VARCHAR,
             number INTEGER,
             builtin_value TEXT GENERATED ALWAYS AS (md5(source)) STORED,
             varying_value TEXT GENERATED ALWAYS AS (md5(varying)) STORED,
             integer_value TEXT GENERATED ALWAYS AS (md5(number)) STORED
         )",
        "INSERT INTO generated_md5_default(source, varying, number) VALUES ('abc', 'abc', 1)",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn generated_md5_builtin_binding_survives_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("md5-binding.uqa");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE SCHEMA md5_persistent",
            "CREATE FUNCTION md5_persistent.md5(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-text'''",
            "SET search_path = md5_persistent, public",
            "CREATE TABLE md5_persistent.generated_values (
                 source TEXT,
                 value TEXT GENERATED ALWAYS AS (md5(source)) STORED
             )",
        ] {
            engine.sql(sql, &[]).unwrap();
        }
    }
    let engine = Engine::open(&database).unwrap();
    engine
        .sql("SET search_path = md5_persistent, pg_catalog, public", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO md5_persistent.generated_values(source) VALUES ('abc')",
            &[],
        )
        .unwrap();
    let result = engine
        .sql("SELECT value FROM md5_persistent.generated_values", &[])
        .unwrap();
    assert_eq!(
        result.rows[0]["value"],
        Value::Str("900150983cd24fb0d6963f7d28e17f72".into())
    );
}
