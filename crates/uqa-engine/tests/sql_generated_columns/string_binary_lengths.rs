//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column parity for one-argument string and binary length overloads.

use super::*;

#[test]
fn generated_columns_bind_string_and_binary_length_overloads() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_lengths (
                 source TEXT,
                 padded CHAR(3),
                 bytes BYTEA,
                 text_chars INTEGER GENERATED ALWAYS AS (length(source)) STORED,
                 padded_chars INTEGER GENERATED ALWAYS AS (char_length(padded)) STORED,
                 padded_bytes INTEGER GENERATED ALWAYS AS (octet_length(padded)) STORED,
                 raw_bytes INTEGER GENERATED ALWAYS AS (length(bytes)) STORED,
                 raw_bits INTEGER GENERATED ALWAYS AS (bit_length(bytes)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_lengths(source, padded, bytes) VALUES ('é', 'a', decode('00ff10', 'hex'))",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT text_chars, padded_chars, padded_bytes, raw_bytes, raw_bits FROM generated_lengths",
            &[],
        )
        .unwrap();
    for (column, expected) in [
        ("text_chars", 1),
        ("padded_chars", 1),
        ("padded_bytes", 3),
        ("raw_bytes", 3),
        ("raw_bits", 24),
    ] {
        assert_eq!(result.rows[0][column], Value::Int(expected), "{column}");
    }

    for sql in [
        "CREATE TABLE invalid_length_integer (source INTEGER, value INTEGER GENERATED ALWAYS AS (length(source)) STORED)",
        "CREATE TABLE invalid_char_length_bytea (source BYTEA, value INTEGER GENERATED ALWAYS AS (char_length(source)) STORED)",
        "CREATE TABLE invalid_octet_length_named (source TEXT, value INTEGER GENERATED ALWAYS AS (octet_length(value => source)) STORED)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
}

#[test]
fn generated_columns_rank_length_user_overloads_by_search_path() {
    let engine = Engine::new();
    for sql in [
        "CREATE SCHEMA length_generated",
        "CREATE FUNCTION length_generated.length(value TEXT) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 101'",
        "CREATE FUNCTION length_generated.length(value VARCHAR) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 102'",
        "CREATE FUNCTION length_generated.length(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 103'",
        "SET search_path = length_generated, public",
        "CREATE TABLE generated_length_default (
             source TEXT,
             varying VARCHAR,
             number INTEGER,
             bytes BYTEA,
             builtin_value INTEGER GENERATED ALWAYS AS (length(source)) STORED,
             varying_value INTEGER GENERATED ALWAYS AS (length(varying)) STORED,
             integer_value INTEGER GENERATED ALWAYS AS (length(number)) STORED,
             bytea_value INTEGER GENERATED ALWAYS AS (length(bytes)) STORED
         )",
        "INSERT INTO generated_length_default(source, varying, number, bytes) VALUES ('abc', 'abc', 1, decode('00ff', 'hex'))",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    let result = engine
        .sql(
            "SELECT builtin_value, varying_value, integer_value, bytea_value FROM generated_length_default",
            &[],
        )
        .unwrap();
    for (column, expected) in [
        ("builtin_value", 3),
        ("varying_value", 102),
        ("integer_value", 103),
        ("bytea_value", 2),
    ] {
        assert_eq!(result.rows[0][column], Value::Int(expected), "{column}");
    }

    engine
        .sql(
            "SET search_path = length_generated, pg_catalog, public",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_length_default(source, varying, number, bytes) VALUES ('é', 'def', 2, decode('0010', 'hex'))",
            &[],
        )
        .unwrap();
    let stable = engine
        .sql(
            "SELECT builtin_value, varying_value, integer_value, bytea_value FROM generated_length_default WHERE source = 'é'",
            &[],
        )
        .unwrap();
    for (column, expected) in [
        ("builtin_value", 1),
        ("varying_value", 102),
        ("integer_value", 103),
        ("bytea_value", 2),
    ] {
        assert_eq!(stable.rows[0][column], Value::Int(expected), "{column}");
    }
}

#[test]
fn generated_length_character_binding_survives_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("length-binding.uqa");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE TABLE generated_length_persistent (
                     padded CHAR(3),
                     bytes BYTEA,
                     padded_bytes INTEGER GENERATED ALWAYS AS (octet_length(padded)) STORED,
                     raw_bytes INTEGER GENERATED ALWAYS AS (length(bytes)) STORED
                 )",
                &[],
            )
            .unwrap();
    }
    let engine = Engine::open(&database).unwrap();
    engine
        .sql(
            "INSERT INTO generated_length_persistent(padded, bytes) VALUES ('a', decode('00ff10', 'hex'))",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT padded_bytes, raw_bytes FROM generated_length_persistent",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["padded_bytes"], Value::Int(3));
    assert_eq!(result.rows[0]["raw_bytes"], Value::Int(3));
}
