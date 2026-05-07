//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! 1:1 port of `uqa/tests/test_datetime.py`.
//!
//! The Rust core `Value` surface stores date/time values as ISO strings,
//! so these tests assert the same observable SQL semantics through that
//! representation rather than Python `date` / `datetime` object types.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn str_value(value: &Value) -> &str {
    match value {
        Value::Str(s) => s,
        other => panic!("expected string, got {other:?}"),
    }
}

fn int_value(value: &Value) -> i64 {
    match value {
        Value::Int(n) => *n,
        other => panic!("expected int, got {other:?}"),
    }
}

fn float_value(value: &Value) -> f64 {
    match value {
        Value::Float(n) => *n,
        other => panic!("expected float, got {other:?}"),
    }
}

fn bool_value(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        other => panic!("expected bool, got {other:?}"),
    }
}

fn ts_table() -> Engine {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE log (id INTEGER, ts TIMESTAMP)");
    exec(
        &engine,
        "INSERT INTO log (id, ts) VALUES (1, '2024-06-15T10:30:45')",
    );
    engine
}

#[test]
fn create_table_with_date() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE events (id INTEGER, event_date DATE)");
    let result = exec(
        &engine,
        "INSERT INTO events (id, event_date) VALUES (1, '2024-01-15')",
    );
    assert_eq!(result.affected_rows, 1);
}

#[test]
fn insert_date_values() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE log (id INTEGER, ts TIMESTAMP)");
    exec(
        &engine,
        "INSERT INTO log (id, ts) VALUES (1, '2024-06-15T10:30:00')",
    );
    exec(
        &engine,
        "INSERT INTO log (id, ts) VALUES (2, '2024-06-16T14:00:00')",
    );
    let result = exec(&engine, "SELECT COUNT(*) AS cnt FROM log");
    assert_eq!(result.rows[0]["cnt"], Value::Int(2));
}

#[test]
fn date_comparison() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE events (id INTEGER, event_date DATE)");
    exec(
        &engine,
        "INSERT INTO events (id, event_date) VALUES (1, '2024-01-01')",
    );
    exec(
        &engine,
        "INSERT INTO events (id, event_date) VALUES (2, '2024-06-15')",
    );
    exec(
        &engine,
        "INSERT INTO events (id, event_date) VALUES (3, '2024-12-31')",
    );
    let result = exec(
        &engine,
        "SELECT id FROM events WHERE event_date > '2024-03-01'",
    );
    let ids: std::collections::BTreeSet<_> = result
        .rows
        .iter()
        .map(|row| int_value(&row["id"]))
        .collect();
    assert_eq!(ids, [2, 3].into_iter().collect());
}

#[test]
fn date_ordering() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE events (id INTEGER, event_date DATE)");
    exec(
        &engine,
        "INSERT INTO events (id, event_date) VALUES (1, '2024-12-31')",
    );
    exec(
        &engine,
        "INSERT INTO events (id, event_date) VALUES (2, '2024-01-01')",
    );
    exec(
        &engine,
        "INSERT INTO events (id, event_date) VALUES (3, '2024-06-15')",
    );
    let result = exec(
        &engine,
        "SELECT id, event_date FROM events ORDER BY event_date ASC",
    );
    let ids: Vec<_> = result
        .rows
        .iter()
        .map(|row| int_value(&row["id"]))
        .collect();
    assert_eq!(ids, vec![2, 3, 1]);
}

#[test]
fn now_returns_timestamp_string() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT NOW() AS ts");
    assert!(str_value(&result.rows[0]["ts"]).contains('T'));
}

#[test]
fn current_date_returns_date_string() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT CURRENT_DATE AS d");
    assert_eq!(str_value(&result.rows[0]["d"]).len(), 10);
}

#[test]
fn current_timestamp_returns_timestamp_string() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT CURRENT_TIMESTAMP AS ts");
    assert!(str_value(&result.rows[0]["ts"]).contains('T'));
}

#[test]
fn extract_year() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT EXTRACT(year FROM ts) AS y FROM log");
    assert_eq!(result.rows[0]["y"], Value::Int(2024));
}

#[test]
fn extract_month() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT EXTRACT(month FROM ts) AS m FROM log");
    assert_eq!(result.rows[0]["m"], Value::Int(6));
}

#[test]
fn extract_day() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT EXTRACT(day FROM ts) AS d FROM log");
    assert_eq!(result.rows[0]["d"], Value::Int(15));
}

#[test]
fn extract_hour() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT EXTRACT(hour FROM ts) AS h FROM log");
    assert_eq!(result.rows[0]["h"], Value::Int(10));
}

#[test]
fn extract_dow() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT EXTRACT(dow FROM ts) AS dow FROM log");
    assert_eq!(result.rows[0]["dow"], Value::Int(6));
}

#[test]
fn extract_epoch() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT EXTRACT(epoch FROM ts) AS e FROM log");
    assert!(float_value(&result.rows[0]["e"]) > 0.0);
}

#[test]
fn date_part() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT DATE_PART('year', ts) AS y FROM log");
    assert_eq!(result.rows[0]["y"], Value::Int(2024));
}

#[test]
fn date_trunc_year() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT DATE_TRUNC('year', ts) AS t FROM log");
    assert!(str_value(&result.rows[0]["t"]).starts_with("2024-01-01T00:00:00"));
}

#[test]
fn date_trunc_month() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT DATE_TRUNC('month', ts) AS t FROM log");
    assert!(str_value(&result.rows[0]["t"]).starts_with("2024-06-01T00:00:00"));
}

#[test]
fn date_trunc_day() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT DATE_TRUNC('day', ts) AS t FROM log");
    assert!(str_value(&result.rows[0]["t"]).starts_with("2024-06-15T00:00:00"));
}

#[test]
fn extract_quarter() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT EXTRACT(quarter FROM ts) AS q FROM log");
    assert_eq!(result.rows[0]["q"], Value::Int(2));
}

#[test]
fn extract_week() {
    let engine = ts_table();
    let result = exec(&engine, "SELECT EXTRACT(week FROM ts) AS w FROM log");
    let week = int_value(&result.rows[0]["w"]);
    assert!((1..=53).contains(&week));
}

#[test]
fn make_timestamp_basic() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT make_timestamp(2024, 3, 15, 10, 30, 0) AS ts",
    );
    assert!(str_value(&result.rows[0]["ts"]).starts_with("2024-03-15T10:30:00"));
}

#[test]
fn make_timestamp_with_fractional_seconds() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT make_timestamp(2024, 1, 1, 0, 0, 30.5) AS ts",
    );
    assert!(str_value(&result.rows[0]["ts"]).starts_with("2024-01-01T00:00:30"));
}

#[test]
fn make_timestamp_midnight() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT make_timestamp(2024, 12, 31, 0, 0, 0) AS ts",
    );
    assert!(str_value(&result.rows[0]["ts"]).starts_with("2024-12-31T00:00:00"));
}

#[test]
fn make_timestamp_end_of_day() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT make_timestamp(2024, 6, 15, 23, 59, 59) AS ts",
    );
    assert!(str_value(&result.rows[0]["ts"]).starts_with("2024-06-15T23:59:59"));
}

#[test]
fn make_interval_days_hours_minutes() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT make_interval(0, 0, 0, 1, 2, 30, 0) AS iv");
    assert!(str_value(&result.rows[0]["iv"]).contains("26:30:00"));
}

#[test]
fn make_interval_hours_minutes_only() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT make_interval(0, 0, 0, 0, 1, 30, 0) AS iv");
    assert!(str_value(&result.rows[0]["iv"]).contains("01:30:00"));
}

#[test]
fn make_interval_zero_interval() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT make_interval(0, 0, 0, 0, 0, 0, 0) AS iv");
    assert!(str_value(&result.rows[0]["iv"]).contains("00:00:00"));
}

#[test]
fn to_number_with_currency_and_commas() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT to_number('$1,234.56', '9999.99') AS n");
    assert!((float_value(&result.rows[0]["n"]) - 1234.56).abs() < 0.01);
}

#[test]
fn to_number_plain_integer() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT to_number('42', '99') AS n");
    assert_eq!(result.rows[0]["n"], Value::Float(42.0));
}

#[test]
fn to_number_negative_number() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT to_number('-99.5', '999.9') AS n");
    assert!((float_value(&result.rows[0]["n"]) - (-99.5)).abs() < 0.01);
}

#[test]
fn to_number_with_spaces() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT to_number('  100  ', '999') AS n");
    assert!((float_value(&result.rows[0]["n"]) - 100.0).abs() < 0.01);
}

#[test]
fn overlaps_overlapping_ranges() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT ('2024-01-01'::timestamp, '2024-06-01'::timestamp) OVERLAPS
                ('2024-03-01'::timestamp, '2024-09-01'::timestamp) AS ov",
    );
    assert!(bool_value(&result.rows[0]["ov"]));
}

#[test]
fn overlaps_non_overlapping_ranges() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT ('2024-01-01'::timestamp, '2024-03-01'::timestamp) OVERLAPS
                ('2024-06-01'::timestamp, '2024-09-01'::timestamp) AS ov",
    );
    assert!(!bool_value(&result.rows[0]["ov"]));
}

#[test]
fn overlaps_adjacent_ranges_do_not_overlap() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT ('2024-01-01'::timestamp, '2024-03-01'::timestamp) OVERLAPS
                ('2024-03-01'::timestamp, '2024-06-01'::timestamp) AS ov",
    );
    assert!(!bool_value(&result.rows[0]["ov"]));
}

#[test]
fn overlaps_function_form() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT overlaps(
            '2024-01-01'::timestamp, '2024-06-01'::timestamp,
            '2024-03-01'::timestamp, '2024-09-01'::timestamp) AS ov",
    );
    assert!(bool_value(&result.rows[0]["ov"]));
}

#[test]
fn overlaps_function_form_non_overlapping() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT overlaps(
            '2024-01-01'::timestamp, '2024-02-01'::timestamp,
            '2024-06-01'::timestamp, '2024-07-01'::timestamp) AS ov",
    );
    assert!(!bool_value(&result.rows[0]["ov"]));
}

#[test]
fn overlaps_one_range_within_another() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT ('2024-01-01'::timestamp, '2024-12-31'::timestamp) OVERLAPS
                ('2024-03-01'::timestamp, '2024-06-01'::timestamp) AS ov",
    );
    assert!(bool_value(&result.rows[0]["ov"]));
}
