//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column parity for `gamma(float8)` and `lgamma(float8)` binding.

use super::*;

fn generated_float(result: &uqa_engine::SQLResult, column: &str) -> f64 {
    match &result.rows[0][column] {
        Value::Float(value) => *value,
        other => panic!("expected double precision column `{column}`, got {other:?}"),
    }
}

#[test]
fn generated_columns_bind_gamma_signatures_and_errors() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_gamma_values (
                 source DOUBLE PRECISION,
                 gamma_value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(source)) STORED,
                 lgamma_value DOUBLE PRECISION GENERATED ALWAYS AS (lgamma(source)) STORED,
                 null_value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(NULL)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_gamma_values(source) VALUES (-.5::float8)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT gamma_value, lgamma_value, null_value FROM generated_gamma_values",
            &[],
        )
        .unwrap();
    assert_eq!(
        generated_float(&result, "gamma_value").to_bits(),
        0xc00c_5bf8_91b4_ef6a
    );
    assert_eq!(
        generated_float(&result, "lgamma_value").to_bits(),
        0x3ff4_3f89_a3f0_edd6
    );
    assert_eq!(result.rows[0]["null_value"], Value::Null);

    for sql in [
        "CREATE TABLE invalid_gamma_boolean (source BOOLEAN, value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(source)) STORED)",
        "CREATE TABLE invalid_gamma_text (source TEXT, value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(source)) STORED)",
        "CREATE TABLE invalid_gamma_array (source INTEGER[], value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(source)) STORED)",
        "CREATE TABLE invalid_gamma_arity (source INTEGER, value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(source, source)) STORED)",
        "CREATE TABLE invalid_gamma_named (source DOUBLE PRECISION, value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(value => source)) STORED)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
}

#[test]
fn generated_gamma_bindings_preserve_search_path_selection() {
    let engine = Engine::new();
    for sql in [
        "CREATE SCHEMA gamma_generated",
        "CREATE FUNCTION gamma_generated.gamma(value INTEGER) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 101::float8'",
        "CREATE FUNCTION gamma_generated.gamma(value DOUBLE PRECISION) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 104::float8'",
        "SET search_path = gamma_generated, public",
        "CREATE TABLE generated_gamma_bindings (
             integer_source INTEGER,
             float_source DOUBLE PRECISION,
             user_value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(integer_source)) STORED,
             builtin_value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(float_source)) STORED
         )",
        "INSERT INTO generated_gamma_bindings(integer_source, float_source) VALUES (1, 5)",
        "SET search_path = gamma_generated, pg_catalog, public",
        "INSERT INTO generated_gamma_bindings(integer_source, float_source) VALUES (2, 5)",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    let result = engine
        .sql(
            "SELECT user_value, builtin_value FROM generated_gamma_bindings ORDER BY integer_source",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    for row in &result.rows {
        assert_eq!(row["user_value"], Value::Float(101.0));
        assert_eq!(row["builtin_value"], Value::Float(24.0));
    }
}

#[test]
fn generated_gamma_bindings_survive_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("gamma-binding.uqa");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE SCHEMA gamma_persistent",
            "CREATE FUNCTION gamma_persistent.gamma(value INTEGER) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 101::float8'",
            "CREATE FUNCTION gamma_persistent.gamma(value DOUBLE PRECISION) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 104::float8'",
            "SET search_path = gamma_persistent, public",
            "CREATE TABLE gamma_persistent.generated_values (
                 integer_source INTEGER,
                 float_source DOUBLE PRECISION,
                 user_value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(integer_source)) STORED,
                 builtin_value DOUBLE PRECISION GENERATED ALWAYS AS (gamma(float_source)) STORED
             )",
        ] {
            engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
    }
    let engine = Engine::open(&database).unwrap();
    engine
        .sql(
            "SET search_path = gamma_persistent, pg_catalog, public",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO gamma_persistent.generated_values(integer_source, float_source) VALUES (1, 5)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT user_value, builtin_value FROM gamma_persistent.generated_values",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["user_value"], Value::Float(101.0));
    assert_eq!(result.rows[0]["builtin_value"], Value::Float(24.0));
}
