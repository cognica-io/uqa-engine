//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table functions in FROM + standalone `VALUES` + scalar table
//! function body. Mirrors the canonical UQA behavior's
//! `_build_generate_series` / `_build_unnest` /
//! `_build_regexp_split_to_table` / `_build_json_each` /
//! `_build_json_array_elements` paths.

use uqa_core::Value;
use uqa_engine::Engine;

fn values(result: &uqa_engine::SQLResult, column: &str) -> Vec<Value> {
    result.rows.iter().map(|row| row[column].clone()).collect()
}

#[test]
fn generate_series_basic() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(1, 5) AS t(n)", &[])
        .unwrap();
    assert_eq!(
        values(&r, "n"),
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]
    );
}

#[test]
fn generate_series_relation_alias_is_default_column_alias() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT x FROM generate_series(1, 3) AS x", &[])
        .unwrap();
    assert_eq!(
        values(&r, "x"),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)]
    );
}

#[test]
fn generate_series_with_step() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(0, 10, 3) AS t(n)", &[])
        .unwrap();
    assert_eq!(
        values(&r, "n"),
        vec![Value::Int(0), Value::Int(3), Value::Int(6), Value::Int(9)]
    );
}

#[test]
fn generate_series_descending() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(5, 1, -1) AS t(n)", &[])
        .unwrap();
    assert_eq!(
        values(&r, "n"),
        vec![
            Value::Int(5),
            Value::Int(4),
            Value::Int(3),
            Value::Int(2),
            Value::Int(1)
        ]
    );
}

#[test]
fn generate_series_single_value() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(1, 1) AS t(n)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0]["n"], Value::Int(1));
}

#[test]
fn generate_series_empty_range() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(5, 1) AS t(n)", &[])
        .unwrap();
    assert!(r.rows.is_empty());
}

#[test]
fn unnest_basic() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT val FROM unnest(ARRAY[10, 20, 30]) AS t(val)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert_eq!(
        values(&r, "val"),
        vec![Value::Int(10), Value::Int(20), Value::Int(30)]
    );
}

#[test]
fn unnest_text_array() {
    let eng = Engine::new();
    let r = eng
        .sql(
            "SELECT val FROM unnest(ARRAY['a', 'b', 'c']) AS t(val)",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert_eq!(
        values(&r, "val"),
        vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
            Value::Str("c".into())
        ]
    );
}

#[test]
fn values_in_from_with_aliased_columns() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT id FROM (VALUES (1), (2), (3)) AS t(id)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 3);
}

#[test]
fn standalone_values_returns_rows() {
    let eng = Engine::new();
    let r = eng.sql("VALUES (1, 'a'), (2, 'b')", &[]).unwrap();
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0].get("column1"), Some(&Value::Int(1)));
    assert_eq!(r.rows[1].get("column2"), Some(&Value::Str("b".into())));
}
