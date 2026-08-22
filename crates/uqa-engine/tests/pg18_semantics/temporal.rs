//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Date, time, interval, and temporal-formatting parity tests.

use super::*;

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
