//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 semantics encoded as engine tests.
//!
//! Every expectation in this file was verified against a live
//! `PostgreSQL` 18.4 instance (the `uqa-pg18` differential-testing
//! container driven by `tests/parity/pg18/run_diff.py`); the tests
//! themselves run without docker.

use uqa_core::{ArrayValue, DecimalValue, TemporalValue, Value};
use uqa_engine::Engine;

fn engine() -> Engine {
    Engine::new()
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine.sql(sql, &[]).unwrap();
    let column = result.columns.first().expect("one column").clone();
    result.rows[0].get(&column).cloned().unwrap_or(Value::Null)
}

fn scalar_err(engine: &Engine, sql: &str) -> String {
    engine.sql(sql, &[]).unwrap_err().to_string()
}

fn text(engine: &Engine, sql: &str) -> String {
    match scalar(engine, sql) {
        Value::Str(s) => s,
        Value::Temporal(t) => t.to_sql_string(),
        Value::Decimal(d) => d.to_sql_string(),
        other => panic!("expected text-like value for {sql}, got {other:?}"),
    }
}

fn dec(text: &str) -> Value {
    Value::Decimal(DecimalValue::parse(text).unwrap())
}

fn array(elements: Vec<Value>) -> Value {
    Value::Array(ArrayValue::try_new(elements).unwrap())
}

fn bounded_array(elements: Vec<Value>, lower_bounds: Vec<i32>) -> Value {
    Value::Array(ArrayValue::with_lower_bounds(elements, lower_bounds).unwrap())
}

// ---------------------------------------------------------------------
// Three-valued logic
// ---------------------------------------------------------------------

#[test]
fn null_comparisons_yield_null() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT NULL = NULL"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT 1 = NULL"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT NULL <> 1"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT NULL < 1"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT NOT NULL"), Value::Null);
}

#[test]
fn row_constructors_are_records_and_keep_postgresql_null_comparison_semantics() {
    let eng = engine();

    assert_eq!(
        scalar(&eng, "SELECT ROW(1, 2)"),
        Value::Row(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        scalar(&eng, "SELECT pg_typeof(ROW(1, 2))"),
        Value::Str("record".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT pg_typeof(ARRAY[1, 2])"),
        Value::Str("integer[]".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT ROW(1, NULL) = ROW(1, NULL)"),
        Value::Null
    );
    assert_eq!(scalar(&eng, "SELECT ROW(1, NULL) < ROW(1, 2)"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT ARRAY[1, NULL] = ARRAY[1, NULL]"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT ROW(1, NULL)::text"),
        Value::Str("(1,)".into())
    );
}

#[test]
fn in_list_three_valued() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT 3 IN (1, 2, NULL)"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT 3 NOT IN (1, 2, NULL)"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT 1 IN (1, NULL)"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT 3 NOT IN (1, 2)"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT NULL IN (1, 2)"), Value::Null);
}

#[test]
fn in_subquery_three_valued() {
    let eng = engine();
    eng.sql("CREATE TABLE in_values (v INTEGER)", &[]).unwrap();
    eng.sql("INSERT INTO in_values VALUES (1), (NULL)", &[])
        .unwrap();

    assert_eq!(
        scalar(&eng, "SELECT 3 IN (SELECT v FROM in_values)"),
        Value::Null
    );
    assert_eq!(
        scalar(&eng, "SELECT 3 NOT IN (SELECT v FROM in_values)"),
        Value::Null
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 IN (SELECT v FROM in_values)"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT NULL IN (SELECT v FROM in_values)"),
        Value::Null
    );
    assert_eq!(
        scalar(&eng, "SELECT NULL NOT IN (SELECT v FROM in_values)"),
        Value::Null
    );

    assert_eq!(
        scalar(&eng, "SELECT 3 IN (SELECT v FROM in_values WHERE false)"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT NULL IN (SELECT v FROM in_values WHERE false)"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT NULL NOT IN (SELECT v FROM in_values WHERE false)"
        ),
        Value::Bool(true)
    );
}

#[test]
fn kleene_and_or() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT NULL AND false"), Value::Bool(false));
    assert_eq!(scalar(&eng, "SELECT NULL AND true"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT NULL OR true"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT NULL OR false"), Value::Null);
}

#[test]
fn between_three_valued() {
    let eng = engine();
    // A definite FALSE bound wins over the NULL bound.
    assert_eq!(
        scalar(&eng, "SELECT 2 BETWEEN 3 AND NULL"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 2 BETWEEN NULL AND 1"),
        Value::Bool(false)
    );
    assert_eq!(scalar(&eng, "SELECT 2 BETWEEN NULL AND 3"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT NULL BETWEEN 1 AND 2"), Value::Null);
}

#[test]
fn case_when_null_not_taken() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT CASE WHEN NULL THEN 1 ELSE 2 END"),
        Value::Int(2)
    );
}

#[test]
fn where_treats_null_as_no_match() {
    let eng = engine();
    eng.sql("CREATE TABLE t3vl (id INTEGER, v INTEGER)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO t3vl (id, v) VALUES (1, 5), (2, 7), (3, NULL)",
        &[],
    )
    .unwrap();
    let ids = |sql: &str| -> Vec<i64> {
        let mut out: Vec<i64> = eng
            .sql(sql, &[])
            .unwrap()
            .rows
            .iter()
            .filter_map(|row| match row.get("id") {
                Some(Value::Int(n)) => Some(*n),
                _ => None,
            })
            .collect();
        out.sort_unstable();
        out
    };
    assert_eq!(ids("SELECT id FROM t3vl WHERE v = 5"), vec![1]);
    // NOT (v = 5) must NOT match the NULL row (PostgreSQL 3VL).
    assert_eq!(ids("SELECT id FROM t3vl WHERE NOT (v = 5)"), vec![2]);
    assert_eq!(ids("SELECT id FROM t3vl WHERE v <> 5"), vec![2]);
    assert_eq!(ids("SELECT id FROM t3vl WHERE v NOT IN (5)"), vec![2]);
    assert_eq!(ids("SELECT id FROM t3vl WHERE v IS NULL"), vec![3]);
    assert_eq!(ids("SELECT id FROM t3vl WHERE v IS NOT NULL"), vec![1, 2]);
}

#[test]
fn select_without_from_honors_where() {
    let eng = engine();
    assert_eq!(eng.sql("SELECT 1 WHERE false", &[]).unwrap().rows.len(), 0);
    assert_eq!(eng.sql("SELECT 1 WHERE NULL", &[]).unwrap().rows.len(), 0);
    assert_eq!(eng.sql("SELECT 1 WHERE true", &[]).unwrap().rows.len(), 1);
    assert_eq!(
        scalar(&eng, "SELECT EXISTS (SELECT 1 WHERE false)"),
        Value::Bool(false)
    );
}

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

// ---------------------------------------------------------------------
// String functions
// ---------------------------------------------------------------------

#[test]
fn trim_family_with_character_set() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT trim(both 'x' from 'xxpadxx')"), "pad");
    assert_eq!(text(&eng, "SELECT ltrim('xxpad', 'x')"), "pad");
    assert_eq!(text(&eng, "SELECT rtrim('padxx', 'x')"), "pad");
    assert_eq!(text(&eng, "SELECT btrim('xxpadxx', 'x')"), "pad");
    // The second argument is a character SET, not a substring.
    assert_eq!(text(&eng, "SELECT ltrim('xyxpad', 'xy')"), "pad");
    assert_eq!(text(&eng, "SELECT trim('  pad  ')"), "pad");
}

#[test]
fn left_right_negative_lengths() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT left('hello', -2)"), "hel");
    assert_eq!(text(&eng, "SELECT right('hello', -2)"), "llo");
    assert_eq!(text(&eng, "SELECT left('hello', -7)"), "");
    assert_eq!(text(&eng, "SELECT right('hello', -7)"), "");
}

#[test]
fn split_part_negative_index() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT split_part('a,b,c', ',', -1)"), "c");
    assert_eq!(text(&eng, "SELECT split_part('a,b,c', ',', -2)"), "b");
    assert_eq!(text(&eng, "SELECT split_part('a,b,c', ',', -4)"), "");
    assert!(scalar_err(&eng, "SELECT split_part('a,b,c', ',', 0)")
        .contains("field position must not be zero"));
}

#[test]
fn substring_clamps_window() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT substring('hello', -1, 3)"), "h");
    assert_eq!(text(&eng, "SELECT substr('hello', 0, 3)"), "he");
    assert_eq!(text(&eng, "SELECT substring('hello', 2, 3)"), "ell");
    assert_eq!(text(&eng, "SELECT substring('hello', 2)"), "ello");
    assert!(scalar_err(&eng, "SELECT substring('hello', 2, -1)")
        .contains("negative substring length not allowed"));
}

#[test]
fn new_scalar_functions() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT factorial(5)"), Value::Int(120));
    assert_eq!(scalar(&eng, "SELECT bit_length('abc')"), Value::Int(24));
    assert_eq!(text(&eng, "SELECT to_hex(255)"), "ff");
    assert_eq!(text(&eng, "SELECT to_hex(-1)"), "ffffffff");
    assert_eq!(
        text(&eng, "SELECT to_hex((-1)::bigint)"),
        "ffffffffffffffff"
    );
    eng.sql(
        "CREATE TABLE to_hex_widths (
            i4 INTEGER,
            i8 BIGINT,
            default_hex TEXT DEFAULT to_hex((-1)::bigint),
            CHECK (to_hex(i8) = 'ffffffffffffffff')
        )",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO to_hex_widths (i4, i8) VALUES (-1, -1)", &[])
        .unwrap();
    assert_eq!(
        text(&eng, "SELECT to_hex(i4) FROM to_hex_widths"),
        "ffffffff"
    );
    assert_eq!(
        text(&eng, "SELECT to_hex(i8) FROM to_hex_widths"),
        "ffffffffffffffff"
    );
    assert_eq!(
        text(&eng, "SELECT default_hex FROM to_hex_widths"),
        "ffffffffffffffff"
    );
    eng.sql("CREATE TABLE to_hex_alter (i8 BIGINT)", &[])
        .unwrap();
    eng.sql("INSERT INTO to_hex_alter VALUES (-1)", &[])
        .unwrap();
    eng.sql(
        "ALTER TABLE to_hex_alter ALTER COLUMN i8 TYPE TEXT USING to_hex(i8)",
        &[],
    )
    .unwrap();
    assert_eq!(
        text(&eng, "SELECT i8 FROM to_hex_alter"),
        "ffffffffffffffff"
    );
    assert_eq!(text(&eng, "SELECT quote_ident('select')"), "\"select\"");
    assert_eq!(text(&eng, "SELECT quote_ident('hello')"), "hello");
    assert_eq!(text(&eng, "SELECT quote_ident('Hello')"), "\"Hello\"");
    assert_eq!(
        text(&eng, "SELECT quote_literal('O''Reilly')"),
        "'O''Reilly'"
    );
    assert_eq!(
        scalar(&eng, "SELECT regexp_count('a1b2c3', '[0-9]')"),
        Value::Int(3)
    );
    assert_eq!(
        scalar(&eng, "SELECT regexp_like('hello', 'ell')"),
        Value::Bool(true)
    );
    assert_eq!(scalar(&eng, "SELECT num_nulls(1, NULL, 2)"), Value::Int(1));
    assert_eq!(
        scalar(&eng, "SELECT num_nonnulls(1, NULL, 2)"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(&eng, "SELECT isfinite(date '2024-01-01')"),
        Value::Bool(true)
    );
}

#[test]
fn string_to_array_pg_semantics() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT string_to_array('a,b,c', ',')"),
        array(vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
            Value::Str("c".into())
        ])
    );
    // NULL separator: one element per character.
    assert_eq!(
        scalar(&eng, "SELECT string_to_array('ab', NULL)"),
        array(vec![Value::Str("a".into()), Value::Str("b".into())])
    );
    // Empty separator: whole string; empty input: empty array.
    assert_eq!(
        scalar(&eng, "SELECT string_to_array('abc', '')"),
        array(vec![Value::Str("abc".into())])
    );
    assert_eq!(
        scalar(&eng, "SELECT string_to_array('', ',')"),
        array(vec![])
    );
    // Third argument marks NULL elements.
    assert_eq!(
        scalar(&eng, "SELECT string_to_array('a,b,c', ',', 'b')"),
        array(vec![
            Value::Str("a".into()),
            Value::Null,
            Value::Str("c".into())
        ])
    );
}

// ---------------------------------------------------------------------
// bytea
// ---------------------------------------------------------------------

#[test]
fn decode_produces_bytes() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT decode('YWJj', 'base64')"),
        Value::Bytes(b"abc".to_vec())
    );
    assert_eq!(
        text(&eng, "SELECT encode(decode('YWJj', 'base64'), 'hex')"),
        "616263"
    );
    assert_eq!(text(&eng, "SELECT encode('abc'::bytea, 'base64')"), "YWJj");
    assert_eq!(
        scalar(&eng, "SELECT reverse(decode('00ff10', 'hex'))"),
        Value::Bytes(vec![0x10, 0xff, 0x00])
    );
}

// ---------------------------------------------------------------------
// INTERVAL
// ---------------------------------------------------------------------

#[test]
fn interval_literals_render_like_pg() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT interval '25 hours'"), "25:00:00");
    assert_eq!(text(&eng, "SELECT interval '1.5 days'"), "1 day 12:00:00");
    assert_eq!(text(&eng, "SELECT interval '90 minutes'"), "01:30:00");
    assert_eq!(
        text(&eng, "SELECT interval '1 day 3 hours'"),
        "1 day 03:00:00"
    );
    assert_eq!(text(&eng, "SELECT interval '-1 day'"), "-1 days");
    assert_eq!(
        text(&eng, "SELECT interval '-1 day 3 hours'"),
        "-1 days +03:00:00"
    );
    assert_eq!(
        text(&eng, "SELECT interval '1 day -3 hours'"),
        "1 day -03:00:00"
    );
    assert_eq!(text(&eng, "SELECT interval '1.5 mons'"), "1 mon 15 days");
    assert_eq!(text(&eng, "SELECT interval '1-2'"), "1 year 2 mons");
    assert_eq!(text(&eng, "SELECT interval '3 4:05:06'"), "3 days 04:05:06");
    assert_eq!(text(&eng, "SELECT interval '90'"), "00:01:30");
    assert_eq!(
        text(&eng, "SELECT interval '2 years -1 mons'"),
        "1 year 11 mons"
    );
    assert_eq!(text(&eng, "SELECT interval '0'"), "00:00:00");
}

#[test]
fn interval_arithmetic() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT date '2024-01-31' + 1"), "2024-02-01");
    assert_eq!(
        scalar(&eng, "SELECT date '2024-03-01' - date '2024-02-01'"),
        Value::Int(29)
    );
    // Month-aware addition clamps to the end of the month.
    assert_eq!(
        text(&eng, "SELECT date '2024-01-31' + interval '1 month'"),
        "2024-02-29 00:00:00"
    );
    assert_eq!(
        text(&eng, "SELECT date '2024-01-31' + interval '1 day'"),
        "2024-02-01 00:00:00"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT timestamp '2024-01-15 10:30:00' + interval '90 minutes'"
        ),
        "2024-01-15 12:00:00"
    );
    assert_eq!(
        text(&eng, "SELECT interval '1 day' + interval '3 hours'"),
        "1 day 03:00:00"
    );
    assert_eq!(
        text(&eng, "SELECT date '2024-01-15' - interval '1 week'"),
        "2024-01-08 00:00:00"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT timestamp '2024-03-01 00:00:00' - timestamp '2024-01-30 12:30:00'"
        ),
        "30 days 11:30:00"
    );
    assert_eq!(
        text(&eng, "SELECT time '13:45:00' + interval '30 minutes'"),
        "14:15:00"
    );
}

#[test]
fn age_symbolic_decomposition() {
    let eng = engine();
    assert_eq!(
        text(&eng, "SELECT age(date '2024-06-15', date '2023-01-10')"),
        "1 year 5 mons 5 days"
    );
    assert_eq!(
        text(&eng, "SELECT age(date '2024-03-31', date '2024-01-30')"),
        "2 mons 1 day"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT age(timestamp '2024-03-01 10:00:00', timestamp '2024-03-31 08:00:00')"
        ),
        "-29 days -22:00:00"
    );
}

#[test]
fn make_interval_named_arguments() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT make_interval(days => 10)"), "10 days");
    assert_eq!(
        text(
            &eng,
            "SELECT make_interval(years => 1, days => 10, secs => 30.5)"
        ),
        "1 year 10 days 00:00:30.5"
    );
    // Third positional argument is weeks.
    assert_eq!(text(&eng, "SELECT make_interval(0, 0, 1)"), "7 days");
}

#[test]
fn extract_returns_pg_numeric_shapes() {
    let eng = engine();
    assert_eq!(
        scalar(
            &eng,
            "SELECT extract(epoch from timestamp '1970-01-01 00:01:00')"
        ),
        dec("60.000000")
    );
    assert_eq!(
        scalar(&eng, "SELECT extract(epoch from interval '1 minute')"),
        dec("60.000000")
    );
    assert_eq!(
        scalar(&eng, "SELECT extract(hour from time '13:45:00')"),
        Value::Int(13)
    );
    assert_eq!(
        scalar(&eng, "SELECT extract(month from interval '14 months')"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(&eng, "SELECT extract(year from interval '14 months')"),
        Value::Int(1)
    );
    assert_eq!(
        scalar(&eng, "SELECT extract(days from interval '40 days')"),
        Value::Int(40)
    );
    // date_part keeps float8 semantics (integral values collapse).
    assert_eq!(
        scalar(&eng, "SELECT date_part('year', date '2024-06-15')"),
        Value::Int(2024)
    );
}

#[test]
fn timestamp_text_uses_pg_format() {
    let eng = engine();
    assert_eq!(
        text(&eng, "SELECT make_timestamp(2024, 1, 15, 10, 30, 0)"),
        "2024-01-15 10:30:00"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT date_trunc('month', timestamp '2024-06-15 10:30:00')"
        ),
        "2024-06-01 00:00:00"
    );
    assert_eq!(
        text(&eng, "SELECT to_char(date '2024-06-15', 'YYYY-MM-DD')"),
        "2024-06-15"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT to_char(timestamp '2024-06-15 13:05:00', 'HH24:MI')"
        ),
        "13:05"
    );
    assert_eq!(text(&eng, "SELECT to_char(1234.5, '9999.99')"), " 1234.50");
    assert_eq!(text(&eng, "SELECT to_char(2.5::numeric, '9')"), " 3");
    assert_eq!(text(&eng, "SELECT to_char(2.5::float8, '9')"), " 2");
    assert_eq!(text(&eng, "SELECT to_char(-2.5::numeric, '9')"), "-3");
    assert_eq!(text(&eng, "SELECT to_char(1.25::numeric, '9.9')"), " 1.3");
    assert_eq!(text(&eng, "SELECT to_char(12::numeric, 'fm000')"), "012");
    assert_eq!(text(&eng, "SELECT to_char(-1.2::numeric, 'S9')"), "-1");
    assert_eq!(text(&eng, "SELECT to_char(1::numeric, 'FM090')"), "001");
}

#[test]
fn integer_avg_remains_exact_numeric() {
    let eng = engine();
    assert_eq!(
        text(
            &eng,
            "SELECT avg(x) FROM (VALUES (9007199254740992::bigint), (9007199254740993::bigint)) AS t(x)"
        ),
        "9007199254740992.5000"
    );
    assert_eq!(
        text(&eng, "SELECT avg(x) FROM (VALUES (1), (2)) AS t(x)"),
        "1.5000000000000000"
    );
}

#[test]
fn array_dimension_errors_and_concatenation_match_postgresql_18() {
    let eng = engine();
    let bounds = eng.sql("SELECT '[0:-1]={}'::int[]", &[]).unwrap_err();
    assert_eq!(bounds.sqlstate(), Some("2202E"));

    let incompatible = eng
        .sql(
            "SELECT array_cat('[0:0][2:3]={{1,2}}'::int[], '[5:5][9:10]={{3,4}}'::int[])",
            &[],
        )
        .unwrap_err();
    assert_eq!(incompatible.sqlstate(), Some("2202E"));
}

// ---------------------------------------------------------------------
// IS DISTINCT FROM / row comparisons / SIMILAR TO / regex operators
// ---------------------------------------------------------------------

#[test]
fn is_distinct_from_is_null_safe() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT NULL IS DISTINCT FROM NULL"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 IS DISTINCT FROM NULL"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 IS DISTINCT FROM 2"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 IS DISTINCT FROM 1"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT NULL IS NOT DISTINCT FROM NULL"),
        Value::Bool(true)
    );
}

#[test]
fn row_comparisons_are_lexicographic() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT (1, 2) < (1, 3)"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT (1, 2) = (1, 2)"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT (1, NULL) = (1, 2)"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT (1, NULL) = (2, 2)"),
        Value::Bool(false)
    );
    assert_eq!(scalar(&eng, "SELECT (1, 2) < (1, NULL)"), Value::Null);
    // The first element decides before the NULL is reached.
    assert_eq!(
        scalar(&eng, "SELECT (2, 2) < (1, NULL)"),
        Value::Bool(false)
    );
}

#[test]
fn similar_to_translates_sql_regex() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT 'abc' SIMILAR TO 'a(b|c)c'"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 'abc' SIMILAR TO '%(b|d)%'"),
        Value::Bool(true)
    );
    // Anchored over the whole string.
    assert_eq!(
        scalar(&eng, "SELECT 'abc' SIMILAR TO '(b|c)%'"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 'abc' SIMILAR TO 'a_c'"),
        Value::Bool(true)
    );
    // Dot is a literal character in SQL regexes.
    assert_eq!(
        scalar(&eng, "SELECT 'a.c' SIMILAR TO 'a.c'"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 'axc' SIMILAR TO 'a.c'"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 'abc' NOT SIMILAR TO 'a_c'"),
        Value::Bool(false)
    );
}

#[test]
fn posix_regex_operators() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT 'abc' ~ 'a.c'"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT 'abc' ~ 'B'"), Value::Bool(false));
    assert_eq!(scalar(&eng, "SELECT 'abc' ~* 'A.C'"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT 'abc' !~ 'x'"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT 'abc' !~* 'A.C'"), Value::Bool(false));
    assert_eq!(scalar(&eng, "SELECT NULL ~ 'x'"), Value::Null);
}

#[test]
fn between_symmetric_swaps_bounds() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT 2 BETWEEN SYMMETRIC 3 AND 1"),
        Value::Bool(true)
    );
    assert_eq!(scalar(&eng, "SELECT 2 BETWEEN 3 AND 1"), Value::Bool(false));
    assert_eq!(
        scalar(&eng, "SELECT 4 BETWEEN SYMMETRIC 3 AND 1"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 2 BETWEEN SYMMETRIC NULL AND 1"),
        Value::Null
    );
    assert_eq!(
        scalar(&eng, "SELECT 2 NOT BETWEEN SYMMETRIC 3 AND 1"),
        Value::Bool(false)
    );
}

#[test]
fn any_all_over_arrays_are_three_valued() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT 2 = ANY(ARRAY[1,2,3])"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 5 = ANY(ARRAY[1,2,3])"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 5 <> ALL(ARRAY[1,2,3])"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 = ANY(ARRAY[1, NULL])"),
        Value::Bool(true)
    );
    assert_eq!(scalar(&eng, "SELECT 3 = ANY(ARRAY[1, NULL])"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT 3 <> ALL(ARRAY[1, NULL])"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT 3 <> ALL(ARRAY[3, NULL])"),
        Value::Bool(false)
    );
    assert_eq!(scalar(&eng, "SELECT NULL = ANY(ARRAY[1, 2])"), Value::Null);
}

#[test]
fn array_subscripts_and_slices() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT (ARRAY[1, 2, 3])[2]"), Value::Int(2));
    assert_eq!(scalar(&eng, "SELECT (ARRAY[1, 2, 3])[0]"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT (ARRAY[1, 2, 3])[4]"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT (ARRAY[1, 2, 3])[1:2]"),
        array(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        scalar(&eng, "SELECT (ARRAY[1, 2, 3])[2:]"),
        array(vec![Value::Int(2), Value::Int(3)])
    );
    assert_eq!(
        scalar(&eng, "SELECT (regexp_match('foo123', '[0-9]+'))[1]"),
        Value::Str("123".into())
    );
}

#[test]
fn array_bounds_survive_storage_sorting_and_dimension_aware_access() {
    let eng = engine();
    let sorted = bounded_array(vec![Value::Int(1), Value::Int(2), Value::Int(3)], vec![0]);
    assert_eq!(
        scalar(&eng, "SELECT array_sort('[0:2]={3,1,2}'::int[])"),
        sorted
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT array_dims(array_sort('[0:2]={3,1,2}'::int[]))"
        ),
        Value::Str("[0:2]".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT array_lower('[0:2]={3,1,2}'::int[], 1)"),
        Value::Int(0)
    );
    assert_eq!(
        scalar(&eng, "SELECT array_upper('[0:2]={3,1,2}'::int[], 1)"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(&eng, "SELECT ('[0:2]={3,1,2}'::int[])[0]"),
        Value::Int(3)
    );

    eng.sql("CREATE TABLE bounded_arrays (v int[])", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO bounded_arrays VALUES ('[0:2]={3,1,2}'::int[])",
        &[],
    )
    .unwrap();
    assert_eq!(
        scalar(&eng, "SELECT v FROM bounded_arrays"),
        bounded_array(vec![Value::Int(3), Value::Int(1), Value::Int(2)], vec![0],)
    );

    assert_eq!(scalar(&eng, "SELECT (ARRAY[[1,2],[3,4]])[1]"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT (ARRAY[[1,2],[3,4]])[1][2]"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(&eng, "SELECT (ARRAY[[1,2],[3,4]])[1:1][2]"),
        array(vec![Value::List(vec![Value::Int(1), Value::Int(2)])])
    );
}

#[test]
fn array_length_of_empty_array_is_null() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT array_length(ARRAY[]::int[], 1)"),
        Value::Null
    );
    assert_eq!(
        scalar(&eng, "SELECT array_length(ARRAY[1,2,3], 1)"),
        Value::Int(3)
    );
    assert_eq!(
        scalar(&eng, "SELECT cardinality(ARRAY[]::int[])"),
        Value::Int(0)
    );
}

#[test]
fn srf_in_select_list_expands_rows() {
    let eng = engine();
    let result = eng.sql("SELECT generate_series(1, 3)", &[]).unwrap();
    assert_eq!(result.rows.len(), 3);
    let result = eng
        .sql("SELECT jsonb_object_keys('{\"b\":1,\"a\":2}'::jsonb)", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn interval_comparison_and_ordering() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT interval '1 day' < interval '25 hours'"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT interval '1 mon' = interval '30 days'"),
        Value::Bool(true)
    );
}

#[test]
fn interval_column_round_trip() {
    let eng = engine();
    // Interval values survive projection through expressions.
    assert_eq!(
        scalar(&eng, "SELECT (interval '1 day' + interval '1 hour') * 2"),
        Value::Temporal(TemporalValue::Interval {
            months: 0,
            days: 2,
            micros: 2 * 3_600 * 1_000_000,
        })
    );
}

// ---------------------------------------------------------------------
// PostgreSQL 18 additions
// ---------------------------------------------------------------------

#[test]
fn pg18_array_sort_and_reverse() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[3,NULL,1,2])"),
        array(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Null,
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[3,NULL,1,2], true)"),
        array(vec![
            Value::Null,
            Value::Int(3),
            Value::Int(2),
            Value::Int(1),
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[3,NULL,1,2], false, true)"),
        array(vec![
            Value::Null,
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_reverse(ARRAY[[1,2],[3,4]])"),
        array(vec![
            Value::List(vec![Value::Int(3), Value::Int(4)]),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[ARRAY[1,NULL],ARRAY[1,2]])"),
        array(vec![
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(1), Value::Null]),
        ])
    );
}

#[test]
fn pg18_json_strip_nulls_can_strip_array_elements() {
    let eng = engine();
    assert_eq!(
        scalar(
            &eng,
            "SELECT jsonb_strip_nulls('{\"a\":null,\"b\":[1,null,{\"c\":null}]}'::jsonb) = '{\"b\":[1,null,{}]}'::jsonb"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT jsonb_strip_nulls('{\"a\":null,\"b\":[1,null,{\"c\":null}]}'::jsonb, true) = '{\"b\":[1,{}]}'::jsonb"
        ),
        Value::Bool(true)
    );
}

#[test]
fn pg18_jsonb_numbers_use_postgresql_numeric_range() {
    let eng = engine();
    for sql in [
        "SELECT '1e131072'::jsonb",
        "SELECT '1e-16384'::jsonb",
        "SELECT '[1e131072]'::jsonb",
        "SELECT '{\"n\":1e131072}'::jsonb",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"), "{sql}");
    }
    assert_eq!(
        scalar(&eng, "SELECT '1e131071'::jsonb > '0'::jsonb"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT '0e200000'::jsonb = '0'::jsonb"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT json_typeof('1e200000'::json)"),
        Value::Str("number".into())
    );
}

#[test]
fn pg18_casefold_uses_full_unicode_mapping() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT casefold('Straße')"),
        Value::Str("strasse".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT casefold('Σςσ')"),
        Value::Str("σσσ".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT casefold('İIıi')"),
        Value::Str("i\u{307}iıi".into())
    );
}

#[test]
fn pg18_checksums_and_gamma_functions() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT crc32('123456789'::bytea)"),
        Value::Int(3_421_780_262)
    );
    assert_eq!(
        scalar(&eng, "SELECT crc32c('123456789'::bytea)"),
        Value::Int(3_808_858_755)
    );
    for (sql, expected) in [
        ("SELECT gamma(5)", 24.0),
        ("SELECT gamma(0.5)", 1.772_453_850_905_516),
        ("SELECT lgamma(5)", 3.178_053_830_347_945_8),
        ("SELECT lgamma(-0.5)", 1.265_512_123_484_645_4),
    ] {
        let Value::Float(actual) = scalar(&eng, sql) else {
            panic!("expected float from {sql}");
        };
        assert!((actual - expected).abs() < 1e-14, "{sql}: {actual}");
    }
    assert_eq!(
        scalar(&eng, "SELECT gamma('Infinity'::float8)"),
        Value::Float(f64::INFINITY)
    );
    assert_eq!(
        scalar(&eng, "SELECT lgamma('-Infinity'::float8)"),
        Value::Float(f64::INFINITY)
    );
    assert!(matches!(
        scalar(&eng, "SELECT gamma('NaN'::float8)"),
        Value::Float(value) if value.is_nan()
    ));
    for sql in [
        "SELECT gamma('-Infinity'::float8)",
        "SELECT gamma(0::float8)",
        "SELECT gamma(-200.5::float8)",
        "SELECT lgamma(0::float8)",
    ] {
        assert!(scalar_err(&eng, sql).contains("out of range"), "{sql}");
    }
}

#[test]
fn pg18_interval_extract_week_and_negative_quarter() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT extract(week FROM interval '20 days')"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(&eng, "SELECT extract(week FROM interval '-20 days')"),
        Value::Int(-2)
    );
    for months in [-14, -12, -1] {
        assert_eq!(
            scalar(
                &eng,
                &format!("SELECT extract(quarter FROM interval '{months} months')")
            ),
            Value::Int(-1)
        );
    }
}

#[test]
fn pg18_to_number_parses_the_postgresql_roman_prefix() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT to_number(' MCMLXXXIV ', 'RN')"),
        dec("1984")
    );
    assert_eq!(
        scalar(&eng, "SELECT to_number('mcmlxxxiv', 'rn')"),
        dec("1984")
    );
    assert_eq!(scalar(&eng, "SELECT to_number('XIVjunk', 'RN')"), dec("14"));
    assert_eq!(
        scalar(&eng, "SELECT to_number('MMMDCCCLXXXVIIII', 'RN')"),
        dec("3888")
    );
    for input in ["IIII", "MCMCM", "IL", "ABC"] {
        let error = eng
            .sql(&format!("SELECT to_number('{input}', 'RN')"), &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P02"), "{input}: {error}");
        assert!(error.to_string().contains("invalid Roman numeral"));
    }
}

#[test]
fn pg18_uuid_generators_set_rfc_bits_and_monotonic_submillisecond_time() {
    let eng = engine();
    for (sql, version) in [("SELECT uuidv4()", '4'), ("SELECT uuidv7()", '7')] {
        let Value::Str(uuid) = scalar(&eng, sql) else {
            panic!("expected UUID text from {sql}");
        };
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.as_bytes()[14], version as u8);
        assert!(matches!(uuid.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    }
    let mut generated = Vec::new();
    for _ in 0..128 {
        let Value::Str(uuid) = scalar(&eng, "SELECT uuidv7()") else {
            panic!("expected UUIDv7 text");
        };
        generated.push(uuid);
    }
    assert!(
        generated.windows(2).all(|pair| pair[0] < pair[1]),
        "UUIDv7 values must be strictly ascending within a backend"
    );

    let Value::Str(unshifted) = scalar(&eng, "SELECT uuidv7()") else {
        panic!("expected unshifted UUIDv7");
    };
    let Value::Str(shifted) = scalar(&eng, "SELECT uuidv7(interval '1 day')") else {
        panic!("expected shifted UUIDv7");
    };
    assert_eq!(shifted.as_bytes()[14], b'7');
    assert!(unshifted < shifted);
    assert!(scalar_err(&eng, "SELECT uuidv7(interval '-100 years')").contains("out of range"));
}

#[test]
fn pg18_min_and_max_accept_arrays() {
    let eng = engine();
    assert_eq!(
        scalar(
            &eng,
            "SELECT min(v) FROM (VALUES (ARRAY[2,1]),(ARRAY[1,9]),(ARRAY[2,0])) AS q(v)"
        ),
        array(vec![Value::Int(1), Value::Int(9)])
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT max(v) FROM (VALUES (ARRAY[2,1]),(ARRAY[1,9]),(ARRAY[2,0])) AS q(v)"
        ),
        array(vec![Value::Int(2), Value::Int(1)])
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT min(v) FROM (VALUES (ARRAY[1,NULL]),(ARRAY[1,2])) AS q(v)"
        ),
        array(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT max(v) FROM (VALUES (ARRAY[1,NULL]),(ARRAY[1,2])) AS q(v)"
        ),
        array(vec![Value::Int(1), Value::Null])
    );
}

#[test]
fn pg18_regex_functions_accept_named_arguments() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT regexp_like(E'\\n', '[^a]', 'n')"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT regexp_like(E'\\n', '[^\\n]', 'en')"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT regexp_count(pattern => '[a-z]+', string => '123abc456def')"
        ),
        Value::Int(2)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT regexp_replace(replacement => 'X', string => 'abc123def456', pattern => '[0-9]+')"
        ),
        Value::Str("abcXdef456".into())
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT regexp_replace(flags => 'g', replacement => 'X', string => 'abc123def456', pattern => '[0-9]+')"
        ),
        Value::Str("abcXdefX".into())
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT regexp_substr(string => 'abc123', pattern => '([0-9]+)', start => 1, \"N\" => 1, flags => '', subexpr => 1)"
        ),
        Value::Str("123".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT regexp_instr('αβ12γ34','[0-9]+',1,2,0)"),
        Value::Int(6)
    );
    assert_eq!(
        scalar(&eng, "SELECT regexp_instr('αβ12γ34','[0-9]+',1,2,1)"),
        Value::Int(8)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT regexp_replace('abc123','([a-z]+)([0-9]+)',E'\\\\2-\\\\1-\\\\&')"
        ),
        Value::Str("123-abc-abc123".into())
    );
}

#[path = "pg18_semantics/numeric_exactness.rs"]
mod numeric_exactness;

#[path = "pg18_semantics/numeric_power_statistics.rs"]
mod numeric_power_statistics;

#[path = "pg18_semantics/array_containment.rs"]
mod array_containment;

#[path = "pg18_semantics/review_regressions.rs"]
mod review_regressions;
