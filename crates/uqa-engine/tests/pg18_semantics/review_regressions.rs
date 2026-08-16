//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn assert_sqlstate(engine: &Engine, sql: &str, expected: &str) {
    let error = engine.sql(sql, &[]).expect_err(sql);
    assert_eq!(
        error.sqlstate(),
        Some(expected),
        "unexpected error: {error}"
    );
}

#[test]
fn empty_array_growth_and_array_ordering_match_postgresql_18() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT array_append('{}'::int[], 1)"),
        array(vec![Value::Int(1)])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_prepend(1, '{}'::int[])"),
        array(vec![Value::Int(1)])
    );
    for sql in [
        "SELECT ARRAY[1] < ARRAY[2]",
        "SELECT ARRAY[1,2] < ARRAY[[1,2]]",
        "SELECT ARRAY[2,0] > ARRAY[[1,9]]",
        "SELECT ARRAY[1] < '[2:2]={1}'::int[]",
    ] {
        assert_eq!(scalar(&eng, sql), Value::Bool(true), "{sql}");
    }
}

#[test]
fn array_element_cast_uses_the_declared_source_width() {
    let eng = engine();
    assert_eq!(
        text(
            &eng,
            "SELECT encode((ARRAY[1::smallint]::bytea[])[1], 'hex')"
        ),
        "0001"
    );
}

#[test]
fn common_type_selection_keeps_unknown_literals_until_context_resolution() {
    let eng = engine();
    assert_eq!(
        text(&eng, "SELECT pg_typeof(ARRAY['x', 'y'::varchar])"),
        "character varying[]"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT pg_typeof(CASE WHEN true THEN 'x' ELSE 'y'::varchar END)"
        ),
        "character varying"
    );
    assert_eq!(
        text(&eng, "SELECT pg_typeof(COALESCE('x', 'y'::varchar))"),
        "character varying"
    );
}

#[test]
fn common_type_selection_coerces_runtime_values_before_aggregation() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE mixed_common_type (g INTEGER, floating DOUBLE PRECISION, exact NUMERIC)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO mixed_common_type VALUES (1, NULL, 1.25), (1, 2.5, 9.75), (2, NULL, 4.5)",
        &[],
    )
    .unwrap();
    eng.sql("SET work_mem TO '1B'", &[]).unwrap();

    let rows = eng
        .sql(
            "SELECT g,
                    pg_typeof(SUM(COALESCE(floating, exact))) AS sum_type,
                    SUM(COALESCE(floating, exact)) AS coalesced,
                    SUM(CASE WHEN floating IS NULL THEN exact ELSE floating END) AS conditional
             FROM mixed_common_type
             GROUP BY g
             ORDER BY g",
            &[],
        )
        .unwrap()
        .rows;

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["sum_type"], Value::Str("double precision".into()));
    assert_eq!(rows[0]["coalesced"], Value::Float(3.75));
    assert_eq!(rows[0]["conditional"], Value::Float(3.75));
    assert_eq!(rows[1]["coalesced"], Value::Float(4.5));
    assert_eq!(rows[1]["conditional"], Value::Float(4.5));
}

#[test]
fn numeric_parser_and_extreme_scale_arithmetic_match_postgresql_18() {
    let eng = engine();
    assert_sqlstate(&eng, "SELECT '-+1'::numeric", "22P02");
    assert_sqlstate(&eng, "SELECT '+NaN'::numeric", "22P02");
    assert_eq!(
        scalar(&eng, "SELECT 0e200000::numeric = 0"),
        Value::Bool(true)
    );
    let Value::Decimal(product) = scalar(&eng, "SELECT 1e-9000::numeric * 1e-9000::numeric") else {
        panic!("expected numeric product");
    };
    assert!(product.is_zero());
    assert_eq!(product.to_sql_string().len(), 16_385);
}

#[test]
fn invalid_date_fields_and_formats_report_postgresql_sqlstates() {
    let eng = engine();
    assert_sqlstate(&eng, "SELECT make_date(2023, 2, 29)", "22008");
    assert_sqlstate(&eng, "SELECT date '2024-02-30'", "22008");
    assert_sqlstate(&eng, "SELECT date 'not-a-date'", "22007");
}

#[test]
fn escaped_array_literal_whitespace_and_null_text_are_significant() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, r"SELECT ('{\ a}'::text[])[1]"),
        Value::Str(" a".into())
    );
    assert_eq!(
        scalar(&eng, r"SELECT ('{N\ULL}'::text[])[1]"),
        Value::Str("NULL".into())
    );
}

#[test]
fn join_using_and_returning_alias_collisions_report_duplicate_relation() {
    let eng = engine();
    assert_sqlstate(
        &eng,
        "SELECT * FROM (VALUES (1)) AS l(id) FULL JOIN (VALUES (1)) AS r(id) USING (id) AS l",
        "42712",
    );
    eng.sql("CREATE TABLE review_returning (id INTEGER)", &[])
        .unwrap();
    assert_sqlstate(
        &eng,
        "INSERT INTO review_returning VALUES (1) RETURNING WITH (OLD AS review_returning, NEW AS after) after.id",
        "42712",
    );
    assert_sqlstate(
        &eng,
        "INSERT INTO review_returning VALUES (2) RETURNING WITH (OLD AS image, NEW AS image) image.id",
        "42712",
    );
    assert_sqlstate(
        &eng,
        "INSERT INTO review_returning VALUES (3) RETURNING WITH (OLD AS first, OLD AS second) first.id",
        "42601",
    );
}

#[test]
fn numeric_to_char_places_sign_tokens_at_their_postgresql_positions() {
    let eng = engine();
    for (sql, expected) in [
        ("SELECT to_char(12::numeric, 'PL999')", "+  12"),
        ("SELECT to_char(-12::numeric, 'PL999')", "  -12"),
        ("SELECT to_char(12::numeric, 'SG999')", "+ 12"),
        ("SELECT to_char(-12::numeric, 'SG999')", "- 12"),
        ("SELECT to_char(12::numeric, '9SG99')", " +12"),
        ("SELECT to_char(-12::numeric, '9MI99')", " -12"),
        ("SELECT to_char(12::numeric, '9S9.9')", "+12.0"),
        ("SELECT to_char(-12::numeric, '99S.9')", "12.0-"),
        ("SELECT to_char(12::numeric, '9MI99SG')", "  12+"),
        ("SELECT to_char(-12::numeric, '9MI99SG')", " -12-"),
        ("SELECT to_char(1::numeric, 'FM9.9MIPL')", "1.+"),
        ("SELECT to_char(-1::numeric, 'FM9.9MIPL')", "1.-"),
        ("SELECT to_char(-12::numeric, '999S,')", " 12-,"),
        ("SELECT to_char(-12::numeric, '999,S')", " 12-,"),
        ("SELECT to_char(-12::numeric, '999PR,')", " <12>,"),
        ("SELECT to_char(12::numeric, '999PR,')", "  12 ,"),
    ] {
        assert_eq!(text(&eng, sql), expected, "{sql}");
    }
    assert_sqlstate(&eng, "SELECT to_char(12::numeric, 'PR999')", "42601");
    assert_sqlstate(&eng, "SELECT to_char(12::numeric, '9S99MI')", "42601");
}

#[test]
fn to_hex_uses_the_declared_integer_overload_at_every_expression_boundary() {
    let eng = engine();
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
}
