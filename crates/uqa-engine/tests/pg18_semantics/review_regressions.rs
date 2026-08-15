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
