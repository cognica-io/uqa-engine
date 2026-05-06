//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end exercise of the lower → optimise → execute pipeline.
//! `WHERE text_match(...)` and the full WHERE-based boolean algebra now
//! flow through `QueryOptimizer` instead of bypassing the operator tree
//! entirely. The driver re-uses the engine's existing `text_match` /
//! `knn_match` helpers so semantics line up with the legacy direct
//! dispatch path.

use uqa_core::Value;
use uqa_engine::Engine;

fn engine_with_corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT, year INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (id, title, body, year) VALUES \
         (1, 'rust async', 'futures and tokio', 2024), \
         (2, 'rust embedded', 'no_std and cortex_m', 2025), \
         (3, 'python web', 'flask and django', 2024), \
         (4, 'rust web', 'axum tokio hyper', 2025)",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn text_match_through_optimiser_returns_matching_docs() {
    let eng = engine_with_corpus();
    let r = eng
        .sql(
            "SELECT id FROM notes WHERE text_match(body, 'tokio') ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 4]);
}

#[test]
fn intersect_of_text_match_and_filter_runs_through_optimiser() {
    let eng = engine_with_corpus();
    let r = eng
        .sql(
            "SELECT id FROM notes WHERE text_match(body, 'tokio') AND year = 2025 ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![4]);
}

#[test]
fn union_of_two_text_match_signals_runs_through_optimiser() {
    let eng = engine_with_corpus();
    let r = eng
        .sql(
            "SELECT id FROM notes WHERE text_match(title, 'rust') OR text_match(body, 'flask') ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 2, 3, 4]);
}

#[test]
fn negation_through_complement_returns_unmatched_docs() {
    let eng = engine_with_corpus();
    let r = eng
        .sql(
            "SELECT id FROM notes WHERE NOT text_match(title, 'python') ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 2, 4]);
}

#[test]
fn pure_column_filter_lowers_to_filter_node() {
    let eng = engine_with_corpus();
    let r = eng
        .sql("SELECT id FROM notes WHERE year = 2024 ORDER BY id", &[])
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 3]);
}
