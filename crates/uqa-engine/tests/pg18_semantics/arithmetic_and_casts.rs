//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Arithmetic, numeric-width, and cast parity tests.

use super::*;

// ---------------------------------------------------------------------
// Arithmetic guards
// ---------------------------------------------------------------------

#[test]
fn division_by_zero_errors() {
    let eng = engine();
    assert!(scalar_err(&eng, "SELECT 1 / 0").contains("division by zero"));
    assert!(scalar_err(&eng, "SELECT 1.0 / 0").contains("division by zero"));
    assert!(scalar_err(&eng, "SELECT 1.5::float8 / 0").contains("division by zero"));
    assert!(scalar_err(&eng, "SELECT mod(5, 0)").contains("division by zero"));
    assert!(scalar_err(&eng, "SELECT 5 % 0").contains("division by zero"));
}

#[test]
fn bigint_overflow_errors() {
    let eng = engine();
    assert!(scalar_err(&eng, "SELECT 9223372036854775807 + 1").contains("bigint out of range"));
    assert!(scalar_err(&eng, "SELECT -9223372036854775807 - 2").contains("bigint out of range"));
    assert!(scalar_err(&eng, "SELECT 9223372036854775807 * 2").contains("bigint out of range"));
}

#[test]
fn integer_arithmetic_preserves_postgresql_width() {
    let eng = engine();
    assert!(scalar_err(&eng, "SELECT 2147483647 + 1").contains("integer out of range"));
    assert!(
        scalar_err(&eng, "SELECT 32767::smallint + 1::smallint").contains("smallint out of range")
    );
    assert!(scalar_err(&eng, "SELECT (2147483646 + 1) + 1").contains("integer out of range"));
    assert_eq!(
        scalar(&eng, "SELECT 2147483647::bigint + 1"),
        Value::Int(2_147_483_648)
    );
    assert_eq!(
        scalar(&eng, "SELECT 32767::smallint + 1"),
        Value::Int(32_768)
    );
}

#[test]
fn array_concatenation_stays_a_sql_array() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT ARRAY[1,2] || ARRAY[3]"),
        array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
    assert_eq!(
        scalar(&eng, "SELECT ARRAY[1,2] || 3"),
        array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
    assert_eq!(
        scalar(&eng, "SELECT 0 || ARRAY[1,2]"),
        array(vec![Value::Int(0), Value::Int(1), Value::Int(2)])
    );
}

// ---------------------------------------------------------------------
// Casts
// ---------------------------------------------------------------------

#[test]
fn numeric_to_int_rounds_half_away_from_zero() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT 5.5::int"), Value::Int(6));
    assert_eq!(scalar(&eng, "SELECT 5.9::int"), Value::Int(6));
    assert_eq!(scalar(&eng, "SELECT 6.5::int"), Value::Int(7));
    // -5.5::int parses as -(5.5::int) = -6.
    assert_eq!(scalar(&eng, "SELECT -5.5::int"), Value::Int(-6));
}

#[test]
fn float_to_int_rounds_half_to_even() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT 2.5::float8::int"), Value::Int(2));
    assert_eq!(scalar(&eng, "SELECT 3.5::float8::int"), Value::Int(4));
    assert_eq!(scalar(&eng, "SELECT round(2.5::float8)"), Value::Float(2.0));
    assert_eq!(scalar(&eng, "SELECT round(3.5::float8)"), Value::Float(4.0));
    // numeric round stays half-away-from-zero.
    assert_eq!(scalar(&eng, "SELECT round(2.5)"), dec("3"));
}

#[test]
fn string_to_number_casts() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT '  5 '::int"), Value::Int(5));
    assert_eq!(scalar(&eng, "SELECT '5.9'::float8"), Value::Float(5.9));
    assert!(scalar_err(&eng, "SELECT '5.9'::int").contains("invalid input syntax"));
    assert!(scalar_err(&eng, "SELECT ''::int").contains("invalid input syntax"));
    assert!(scalar_err(&eng, "SELECT 'abc'::int").contains("invalid input syntax"));
}

#[test]
fn boolean_cast_follows_parse_bool() {
    let eng = engine();
    for (input, expected) in [
        ("'off'", false),
        ("'of'", false),
        ("'no'", false),
        ("'n'", false),
        ("'0'", false),
        ("'f'", false),
        ("'yes'", true),
        ("'ye'", true),
        ("'on'", true),
        ("'1'", true),
        ("'tr'", true),
        ("' t '", true),
    ] {
        assert_eq!(
            scalar(&eng, &format!("SELECT {input}::boolean")),
            Value::Bool(expected),
            "cast {input}"
        );
    }
    assert!(scalar_err(&eng, "SELECT 'o'::boolean").contains("invalid input syntax"));
}

#[test]
fn char_and_varchar_casts_truncate() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT 'abc'::char(2)"),
        Value::FixedChar("ab".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT 'a'::char(2)"),
        Value::FixedChar("a ".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT 'ab'::varchar(1)"),
        Value::Str("a".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT 123::varchar(2)"),
        Value::Str("12".into())
    );
}

#[test]
fn fixed_character_columns_pad_output_and_ignore_trailing_spaces_in_comparisons() {
    let eng = engine();
    eng.sql("CREATE TABLE fixed_labels (code CHAR(4))", &[])
        .unwrap();
    eng.sql("INSERT INTO fixed_labels VALUES ('x')", &[])
        .unwrap();

    assert_eq!(
        scalar(&eng, "SELECT code FROM fixed_labels"),
        Value::FixedChar("x   ".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT code = 'x' FROM fixed_labels"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT code = 'x  ' FROM fixed_labels"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT code::text FROM fixed_labels"),
        Value::Str("x".into())
    );
}

#[test]
fn numeric_division_selects_postgresql_result_scale() {
    let eng = engine();
    assert_eq!(
        text(&eng, "SELECT 1::numeric / 2::numeric"),
        "0.50000000000000000000"
    );
    assert_eq!(
        text(&eng, "SELECT 37569624.64::numeric / 1478::numeric"),
        "25419.231826792963"
    );
    assert_eq!(
        text(&eng, "SELECT 75.18::numeric / 1478::numeric"),
        "0.05086603518267929635"
    );
}

#[test]
fn array_literal_cast() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT '{1,2,3}'::int[]"),
        array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
    assert_eq!(
        scalar(&eng, "SELECT '{a,\"b c\",NULL}'::text[]"),
        array(vec![
            Value::Str("a".into()),
            Value::Str("b c".into()),
            Value::Null
        ])
    );
}

#[test]
fn integer_range_checks() {
    let eng = engine();
    assert!(scalar_err(&eng, "SELECT 40000::smallint").contains("smallint out of range"));
    assert!(scalar_err(&eng, "SELECT 3000000000::integer").contains("integer out of range"));
    assert_eq!(
        scalar(&eng, "SELECT 3000000000::bigint"),
        Value::Int(3_000_000_000)
    );
}

#[test]
fn unary_minus_preserves_postgresql_18_operand_type_and_overflow() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT pg_typeof(-(1::smallint))"), "smallint");
    assert_eq!(text(&eng, "SELECT pg_typeof(-(1::integer))"), "integer");
    assert_eq!(text(&eng, "SELECT pg_typeof(-(1::bigint))"), "bigint");
    assert_eq!(
        text(&eng, "SELECT encode((-1::smallint)::bytea, 'hex')"),
        "ffff"
    );
    for sql in [
        "SELECT -('-32768'::smallint)",
        "SELECT -('-2147483648'::integer)",
        "SELECT -('-9223372036854775808'::bigint)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"), "{sql}: {error}");
    }
}

#[test]
fn oid_and_xid_casts_preserve_postgresql_18_source_type_rules() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT '-1'::oid"),
        Value::Int(i64::from(u32::MAX))
    );
    assert_eq!(
        scalar(&eng, "SELECT '-2147483648'::xid"),
        Value::Int(i64::from(i32::MIN as u32))
    );
    assert_eq!(
        scalar(&eng, "SELECT (-1::smallint)::oid"),
        Value::Int(i64::from(u32::MAX))
    );
    assert_eq!(
        scalar(&eng, "SELECT (-1::integer)::oid"),
        Value::Int(i64::from(u32::MAX))
    );
    assert_eq!(
        scalar(&eng, "SELECT (4294967295::bigint)::oid"),
        Value::Int(i64::from(u32::MAX))
    );
    let error = eng.sql("SELECT (-1::bigint)::oid", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22003"));
    assert_eq!(error.to_string(), "OID out of range");
    for sql in [
        "SELECT true::oid",
        "SELECT (1.0::numeric)::oid",
        "SELECT (1.0::double precision)::oid",
        "SELECT (1::integer)::xid",
        "SELECT ('1'::oid)::xid",
        "SELECT ('1'::xid)::oid",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42846"), "{sql}: {error}");
    }
    for sql in ["SELECT '-2147483649'::oid", "SELECT '4294967296'::xid"] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"), "{sql}: {error}");
    }

    eng.sql(
        "CREATE TABLE oid_cast_sources (s SMALLINT, i INTEGER, b BIGINT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO oid_cast_sources VALUES (-1, -1, -1)", &[])
        .unwrap();
    let row = eng
        .sql("SELECT s::oid AS s, i::oid AS i FROM oid_cast_sources", &[])
        .unwrap();
    assert_eq!(row.rows[0].get("s"), Some(&Value::Int(i64::from(u32::MAX))));
    assert_eq!(row.rows[0].get("i"), Some(&Value::Int(i64::from(u32::MAX))));
    let error = eng
        .sql("SELECT b::oid FROM oid_cast_sources", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22003"));
    let error = eng
        .sql("SELECT i::xid FROM oid_cast_sources", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42846"));
}
