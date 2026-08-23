//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column parity for `crc32(bytea)` and `crc32c(bytea)` binding.

use super::*;

#[test]
fn generated_columns_bind_crc_checksum_signatures() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_checksums (
                 source BYTEA,
                 crc BIGINT GENERATED ALWAYS AS (crc32(source)) STORED,
                 crc_c BIGINT GENERATED ALWAYS AS (crc32c(source)) STORED,
                 null_crc BIGINT GENERATED ALWAYS AS (crc32(NULL)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_checksums(source) VALUES (decode('00ff10', 'hex'))",
            &[],
        )
        .unwrap();
    let result = engine
        .sql("SELECT crc, crc_c, null_crc FROM generated_checksums", &[])
        .unwrap();
    assert_eq!(result.rows[0]["crc"], Value::Int(1_909_601_284));
    assert_eq!(result.rows[0]["crc_c"], Value::Int(3_554_230_422));
    assert_eq!(result.rows[0]["null_crc"], Value::Null);

    for sql in [
        "CREATE TABLE invalid_crc_text (source TEXT, value BIGINT GENERATED ALWAYS AS (crc32(source)) STORED)",
        "CREATE TABLE invalid_crc_integer (source INTEGER, value BIGINT GENERATED ALWAYS AS (crc32(source)) STORED)",
        "CREATE TABLE invalid_crc_arity (source BYTEA, value BIGINT GENERATED ALWAYS AS (crc32(source, source)) STORED)",
        "CREATE TABLE invalid_crc_named (source BYTEA, value BIGINT GENERATED ALWAYS AS (crc32(value => source)) STORED)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
}

#[test]
fn generated_crc_bindings_preserve_search_path_selection() {
    let engine = Engine::new();
    for sql in [
        "CREATE SCHEMA checksum_generated",
        "CREATE FUNCTION checksum_generated.crc32(value TEXT) RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS 'SELECT 101::bigint'",
        "CREATE FUNCTION checksum_generated.crc32(value BYTEA) RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS 'SELECT 102::bigint'",
        "SET search_path = checksum_generated, public",
        "CREATE TABLE generated_checksum_values (
             bytes BYTEA,
             text_value TEXT,
             builtin_value BIGINT GENERATED ALWAYS AS (crc32(bytes)) STORED,
             user_value BIGINT GENERATED ALWAYS AS (crc32(text_value)) STORED
         )",
        "INSERT INTO generated_checksum_values(bytes, text_value) VALUES (decode('00', 'hex'), 'abc')",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine
        .sql(
            "SET search_path = checksum_generated, pg_catalog, public",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_checksum_values(bytes, text_value) VALUES (decode('01', 'hex'), 'def')",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT builtin_value, user_value FROM generated_checksum_values ORDER BY user_value",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["user_value"], Value::Int(101));
    assert_eq!(result.rows[1]["user_value"], Value::Int(101));
    assert_eq!(result.rows[0]["builtin_value"], Value::Int(3_523_407_757));
    assert_eq!(result.rows[1]["builtin_value"], Value::Int(2_768_625_435));
}

#[test]
fn generated_crc_builtin_binding_survives_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("checksum-binding.uqa");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE SCHEMA checksum_persistent",
            "CREATE FUNCTION checksum_persistent.crc32(value BYTEA) RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS 'SELECT 101::bigint'",
            "SET search_path = checksum_persistent, public",
            "CREATE TABLE checksum_persistent.generated_values (
                 source BYTEA,
                 value BIGINT GENERATED ALWAYS AS (crc32(source)) STORED
             )",
        ] {
            engine.sql(sql, &[]).unwrap();
        }
    }
    let engine = Engine::open(&database).unwrap();
    engine
        .sql(
            "SET search_path = checksum_persistent, pg_catalog, public",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO checksum_persistent.generated_values(source) VALUES (decode('00', 'hex'))",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT value FROM checksum_persistent.generated_values",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["value"], Value::Int(3_523_407_757));
}
