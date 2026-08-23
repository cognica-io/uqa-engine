//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 one-argument string and binary length overload parity.

use super::*;
use uqa_engine::SQLParam;

#[test]
fn pg18_length_functions_preserve_text_character_and_bytea_semantics() {
    let eng = engine();
    for (sql, expected) in [
        ("SELECT length('é')", 1),
        ("SELECT length(decode('00ff10', 'hex'))", 3),
        ("SELECT length('a  '::char(3))", 1),
        ("SELECT char_length('a  '::char(3))", 1),
        ("SELECT character_length('a  '::char(3))", 1),
        ("SELECT octet_length('é')", 2),
        ("SELECT octet_length(decode('00ff10', 'hex'))", 3),
        ("SELECT octet_length('a'::char(3))", 3),
        ("SELECT octet_length('é'::char(3))", 4),
        ("SELECT bit_length('é')", 16),
        ("SELECT bit_length(decode('00ff10', 'hex'))", 24),
        ("SELECT bit_length('a'::char(3))", 8),
        ("SELECT length('abc'::varchar)", 3),
        ("SELECT length('abc'::name)", 3),
        ("SELECT length('a'::\"char\")", 1),
        ("SELECT length((SELECT decode('00ff10', 'hex')))", 3),
        ("SELECT octet_length((SELECT 'a'::char(3)))", 3),
        ("SELECT bit_length((SELECT 'a'::char(3)))", 8),
        ("SELECT pg_catalog.length('é')", 1),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Int(expected), "{sql}");
    }
    for sql in [
        "SELECT pg_typeof(length(NULL))",
        "SELECT pg_typeof(length(NULL::bytea))",
        "SELECT pg_typeof(char_length(NULL))",
        "SELECT pg_typeof(character_length(NULL))",
        "SELECT pg_typeof(octet_length(NULL))",
        "SELECT pg_typeof(bit_length(NULL))",
    ] {
        assert_eq!(scalar(&eng, sql), Value::Str("integer".into()), "{sql}");
    }
    for sql in [
        "SELECT length(NULL)",
        "SELECT length(NULL::bytea)",
        "SELECT length(NULL::char(3))",
        "SELECT char_length(NULL)",
        "SELECT character_length(NULL)",
        "SELECT octet_length(NULL::bytea)",
        "SELECT bit_length(NULL::bytea)",
    ] {
        assert_eq!(scalar(&eng, sql), Value::Null, "{sql}");
    }

    for (parameter, expected) in [
        (Value::Str("é".into()), vec![1, 2, 16]),
        (Value::Bytes(vec![0x00, 0xff, 0x10]), vec![3, 3, 24]),
    ] {
        let result = eng
            .sql(
                "SELECT length($1) AS chars, octet_length($1) AS octets, bit_length($1) AS bits",
                &[SQLParam::Scalar(parameter)],
            )
            .unwrap();
        assert_eq!(result.rows[0]["chars"], Value::Int(expected[0]));
        assert_eq!(result.rows[0]["octets"], Value::Int(expected[1]));
        assert_eq!(result.rows[0]["bits"], Value::Int(expected[2]));
    }
}

#[test]
fn pg18_length_functions_reject_missing_one_argument_overloads() {
    let eng = engine();
    for (sql, signature) in [
        ("SELECT length(1)", "length(integer)"),
        (
            "SELECT char_length(decode('00', 'hex'))",
            "char_length(bytea)",
        ),
        (
            "SELECT character_length(decode('00', 'hex'))",
            "character_length(bytea)",
        ),
        ("SELECT octet_length(1)", "octet_length(integer)"),
        ("SELECT bit_length(1)", "bit_length(integer)"),
        ("SELECT length(value => 'a')", "length(value => unknown)"),
        ("SELECT char_length()", "char_length()"),
        (
            "SELECT bit_length('a', 'b')",
            "bit_length(unknown, unknown)",
        ),
        (
            "SELECT pg_catalog.bit_length(1)",
            "pg_catalog.bit_length(integer)",
        ),
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
fn pg18_length_functions_rank_user_overloads_and_pg_catalog() {
    let eng = engine();
    for sql in [
        "CREATE SCHEMA length_overload",
        "CREATE FUNCTION length_overload.length(value TEXT) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 101'",
        "CREATE FUNCTION length_overload.length(value VARCHAR) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 102'",
        "CREATE FUNCTION length_overload.length(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 103'",
        "CREATE FUNCTION length_overload.octet_length(value TEXT) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 201'",
        "CREATE FUNCTION length_overload.char_length(value TEXT) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 301'",
        "SET search_path = length_overload, public",
    ] {
        eng.sql(sql, &[]).unwrap();
    }
    for (sql, expected) in [
        ("SELECT length('abc'::text)", 3),
        ("SELECT length('abc'::varchar)", 102),
        ("SELECT length('abc')", 3),
        ("SELECT length(1)", 103),
        ("SELECT length(decode('00ff', 'hex'))", 2),
        ("SELECT octet_length('abc'::text)", 3),
        ("SELECT char_length('abc'::text)", 3),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Int(expected), "{sql}");
    }

    eng.sql("SET search_path = length_overload, pg_catalog, public", &[])
        .unwrap();
    for (sql, expected) in [
        ("SELECT length('abc'::text)", 101),
        ("SELECT length('abc'::varchar)", 102),
        ("SELECT length('abc')", 101),
        ("SELECT length(1)", 103),
        ("SELECT length(decode('00ff', 'hex'))", 2),
        ("SELECT octet_length('abc'::text)", 201),
        ("SELECT char_length('abc'::text)", 301),
        ("SELECT pg_catalog.length('abc')", 3),
        ("SELECT length_overload.length('abc')", 101),
    ] {
        assert_eq!(scalar(&eng, sql), Value::Int(expected), "{sql}");
    }
}
