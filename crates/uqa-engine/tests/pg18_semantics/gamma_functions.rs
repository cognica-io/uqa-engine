//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 parity for `gamma(float8)` and `lgamma(float8)`.

use super::*;
use uqa_engine::SQLParam;

fn float(engine: &Engine, sql: &str) -> f64 {
    match scalar(engine, sql) {
        Value::Float(value) => value,
        other => panic!("expected double precision from {sql}, got {other:?}"),
    }
}

#[test]
fn pg18_gamma_functions_preserve_native_results_and_float8_types() {
    let engine = engine();
    for (sql, expected_bits) in [
        ("SELECT gamma(5)", 0x4038_0000_0000_0000),
        ("SELECT gamma(.5::float8)", 0x3ffc_5bf8_91b4_ef6b),
        ("SELECT gamma(-.5::float8)", 0xc00c_5bf8_91b4_ef6a),
        ("SELECT lgamma(-.5::float8)", 0x3ff4_3f89_a3f0_edd6),
        ("SELECT gamma((SELECT .5::float8))", 0x3ffc_5bf8_91b4_ef6b),
    ] {
        assert_eq!(float(&engine, sql).to_bits(), expected_bits, "{sql}");
    }
    for sql in [
        "SELECT pg_typeof(gamma(5::smallint))",
        "SELECT pg_typeof(gamma(5::integer))",
        "SELECT pg_typeof(gamma(5::bigint))",
        "SELECT pg_typeof(gamma(5::numeric))",
        "SELECT pg_typeof(gamma(5::real))",
        "SELECT pg_typeof(gamma(5::double precision))",
        "SELECT pg_typeof(gamma('5'))",
        "SELECT pg_typeof(lgamma(NULL))",
    ] {
        assert_eq!(
            scalar(&engine, sql),
            Value::Str("double precision".into()),
            "{sql}"
        );
    }
    assert_eq!(scalar(&engine, "SELECT gamma(NULL)"), Value::Null);
    assert_eq!(float(&engine, "SELECT pg_catalog.gamma(5)"), 24.0);
    for (parameter, expected) in [
        (Value::Int(5), Some(24.0)),
        (
            Value::Float(0.5),
            Some(f64::from_bits(0x3ffc_5bf8_91b4_ef6b)),
        ),
        (Value::Null, None),
    ] {
        let result = engine
            .sql(
                "SELECT gamma($1) AS value, pg_typeof(gamma($1)) AS ty",
                &[SQLParam::Scalar(parameter.clone())],
            )
            .unwrap_or_else(|error| panic!("{parameter:?}: {error}"));
        assert_eq!(result.rows[0]["ty"], Value::Str("double precision".into()));
        match expected {
            Some(expected) => assert_eq!(result.rows[0]["value"], Value::Float(expected)),
            None => assert_eq!(result.rows[0]["value"], Value::Null),
        }
    }
}

#[test]
fn pg18_gamma_functions_match_special_values_and_errors() {
    let engine = engine();
    assert_eq!(
        scalar(&engine, "SELECT gamma('Infinity'::float8)"),
        Value::Float(f64::INFINITY)
    );
    assert_eq!(
        scalar(&engine, "SELECT lgamma('-Infinity'::float8)"),
        Value::Float(f64::INFINITY)
    );
    assert!(matches!(
        scalar(&engine, "SELECT gamma('NaN'::float8)"),
        Value::Float(value) if value.is_nan()
    ));
    assert!(matches!(
        scalar(&engine, "SELECT lgamma('NaN'::float8)"),
        Value::Float(value) if value.is_nan()
    ));
    for sql in [
        "SELECT gamma('-Infinity'::float8)",
        "SELECT gamma(0::float8)",
        "SELECT gamma(-1::float8)",
        "SELECT gamma(172::float8)",
        "SELECT gamma(-200.5::float8)",
        "SELECT lgamma(0::float8)",
        "SELECT lgamma(-1::float8)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"), "{sql}: {error}");
    }
    let error = engine.sql("SELECT gamma('abc')", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22P02"));
}

#[test]
fn pg18_gamma_functions_reject_missing_overloads() {
    let engine = engine();
    for (sql, signature) in [
        ("SELECT gamma()", "gamma()"),
        ("SELECT gamma(1, 2)", "gamma(integer, integer)"),
        ("SELECT gamma(true)", "gamma(boolean)"),
        ("SELECT gamma('1'::text)", "gamma(text)"),
        ("SELECT gamma(ARRAY[1])", "gamma(integer[])"),
        (
            "SELECT gamma(value => .5::float8)",
            "gamma(value => double precision)",
        ),
        (
            "SELECT pg_catalog.lgamma('1'::text)",
            "pg_catalog.lgamma(text)",
        ),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
        assert_eq!(
            error.to_string(),
            format!("function {signature} does not exist"),
            "{sql}"
        );
    }
}

#[test]
fn pg18_gamma_functions_rank_user_overloads_and_pg_catalog_order() {
    let engine = engine();
    for sql in [
        "CREATE SCHEMA gamma_overload",
        "CREATE FUNCTION gamma_overload.gamma(value INTEGER) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 101::float8'",
        "CREATE FUNCTION gamma_overload.gamma(value NUMERIC) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 102::float8'",
        "CREATE FUNCTION gamma_overload.gamma(value REAL) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 103::float8'",
        "CREATE FUNCTION gamma_overload.gamma(value DOUBLE PRECISION) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 104::float8'",
        "CREATE FUNCTION gamma_overload.gamma(value TEXT) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 105::float8'",
        "CREATE FUNCTION gamma_overload.lgamma(value INTEGER) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 201::float8'",
        "SET search_path = gamma_overload, public",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    for (sql, expected) in [
        ("SELECT gamma(1::smallint)", 1.0),
        ("SELECT gamma(1::integer)", 101.0),
        ("SELECT gamma(1::bigint)", 1.0),
        ("SELECT gamma(1::numeric)", 102.0),
        ("SELECT gamma(1::real)", 103.0),
        ("SELECT gamma(1::double precision)", 1.0),
        ("SELECT gamma('1')", 105.0),
        ("SELECT gamma(value => 1)", 101.0),
        ("SELECT lgamma(1)", 201.0),
        ("SELECT pg_catalog.gamma(1)", 1.0),
        ("SELECT gamma_overload.gamma(1)", 101.0),
    ] {
        assert_eq!(float(&engine, sql), expected, "{sql}");
    }

    engine
        .sql("SET search_path = gamma_overload, pg_catalog, public", &[])
        .unwrap();
    for (sql, expected) in [
        ("SELECT gamma(1::smallint)", 104.0),
        ("SELECT gamma(1::integer)", 101.0),
        ("SELECT gamma(1::bigint)", 104.0),
        ("SELECT gamma(1::numeric)", 102.0),
        ("SELECT gamma(1::real)", 103.0),
        ("SELECT gamma(1::double precision)", 104.0),
        ("SELECT gamma('1')", 105.0),
    ] {
        assert_eq!(float(&engine, sql), expected, "{sql}");
    }
}

#[test]
fn pg18_gamma_unknowns_preserve_cross_category_ambiguity() {
    let engine = engine();
    for sql in [
        "CREATE SCHEMA gamma_ambiguous",
        "CREATE FUNCTION gamma_ambiguous.gamma(value UUID) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 101::float8'",
        "CREATE FUNCTION gamma_ambiguous.gamma(value JSON) RETURNS DOUBLE PRECISION LANGUAGE SQL IMMUTABLE AS 'SELECT 102::float8'",
        "SET search_path = gamma_ambiguous, public",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    let error = engine.sql("SELECT gamma('abc')", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42725"));
    assert_eq!(error.to_string(), "function gamma(unknown) is not unique");
}
