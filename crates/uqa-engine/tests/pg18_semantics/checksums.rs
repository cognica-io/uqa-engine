//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `crc32(bytea)` and `crc32c(bytea)` parity.

use super::*;
use uqa_engine::SQLParam;

#[test]
fn pg18_crc_checksums_preserve_bytea_signatures() {
    let eng = engine();
    for (sql, expected) in [
        ("SELECT crc32('abc')", 891_568_578),
        ("SELECT crc32c('abc')", 910_901_175),
        ("SELECT crc32(decode('00ff10', 'hex'))", 1_909_601_284),
        ("SELECT crc32c(decode('00ff10', 'hex'))", 3_554_230_422),
        ("SELECT pg_catalog.crc32('abc')", 891_568_578),
        (
            "SELECT crc32((SELECT decode('00ff10', 'hex')))",
            1_909_601_284,
        ),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Int(expected), "{sql}");
    }
    for sql in [
        "SELECT pg_typeof(crc32(NULL))",
        "SELECT pg_typeof(crc32c(NULL::bytea))",
        "SELECT pg_typeof(crc32((SELECT decode('00ff10', 'hex'))))",
    ] {
        assert_eq!(scalar(&eng, sql), Value::Str("bigint".into()), "{sql}");
    }
    assert_eq!(scalar(&eng, "SELECT crc32(NULL)"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT crc32c(NULL::bytea)"), Value::Null);

    for parameter in [Value::Bytes(vec![0x00, 0xff, 0x10]), Value::Null] {
        let result = eng
            .sql(
                "SELECT crc32($1) AS value, pg_typeof(crc32($1)) AS ty",
                &[SQLParam::Scalar(parameter.clone())],
            )
            .unwrap_or_else(|error| panic!("{parameter:?}: {error}"));
        let expected = if matches!(parameter, Value::Null) {
            Value::Null
        } else {
            Value::Int(1_909_601_284)
        };
        assert_eq!(result.rows[0]["value"], expected);
        assert_eq!(result.rows[0]["ty"], Value::Str("bigint".into()));
    }
}

#[test]
fn pg18_crc_checksums_reject_missing_overloads() {
    let eng = engine();
    for (sql, signature) in [
        ("SELECT crc32()", "crc32()"),
        ("SELECT crc32(1)", "crc32(integer)"),
        ("SELECT crc32('abc'::text)", "crc32(text)"),
        ("SELECT crc32('abc'::varchar)", "crc32(character varying)"),
        (
            "SELECT crc32(decode('00', 'hex'), decode('01', 'hex'))",
            "crc32(bytea, bytea)",
        ),
        (
            "SELECT crc32(value => decode('00', 'hex'))",
            "crc32(value => bytea)",
        ),
        ("SELECT crc32c(ARRAY[1])", "crc32c(integer[])"),
        ("SELECT pg_catalog.crc32c(1)", "pg_catalog.crc32c(integer)"),
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
fn pg18_crc_checksums_rank_user_overloads_and_pg_catalog_order() {
    let eng = engine();
    for sql in [
        "CREATE SCHEMA checksum_overload",
        "CREATE FUNCTION checksum_overload.crc32(value TEXT) RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS 'SELECT 101::bigint'",
        "CREATE FUNCTION checksum_overload.crc32(value INTEGER) RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS 'SELECT 102::bigint'",
        "CREATE FUNCTION checksum_overload.crc32(value BYTEA) RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS 'SELECT 103::bigint'",
        "CREATE FUNCTION checksum_overload.crc32c(value TEXT) RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS 'SELECT 104::bigint'",
        "SET search_path = checksum_overload, public",
    ] {
        eng.sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    for (sql, expected) in [
        ("SELECT crc32('abc')", 101),
        ("SELECT crc32('abc'::text)", 101),
        ("SELECT crc32(1)", 102),
        ("SELECT crc32(decode('00', 'hex'))", 3_523_407_757),
        ("SELECT crc32c('abc')", 104),
        ("SELECT crc32c(decode('00', 'hex'))", 1_383_945_041),
        ("SELECT pg_catalog.crc32('abc')", 891_568_578),
        ("SELECT checksum_overload.crc32('abc')", 101),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Int(expected), "{sql}");
    }

    eng.sql(
        "SET search_path = checksum_overload, pg_catalog, public",
        &[],
    )
    .unwrap();
    assert_eq!(
        scalar(&eng, "SELECT crc32(decode('00', 'hex'))"),
        Value::Int(103)
    );
}

#[test]
fn pg18_crc_checksum_unknowns_preserve_cross_schema_ambiguity() {
    let eng = engine();
    for sql in [
        "CREATE SCHEMA checksum_ambiguous",
        "CREATE FUNCTION checksum_ambiguous.crc32(value UUID) RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS 'SELECT 101::bigint'",
        "CREATE FUNCTION checksum_ambiguous.crc32(value JSON) RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS 'SELECT 102::bigint'",
        "SET search_path = checksum_ambiguous, public",
    ] {
        eng.sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    let error = eng.sql("SELECT crc32('abc')", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42725"));
    assert_eq!(error.to_string(), "function crc32(unknown) is not unique");
}
