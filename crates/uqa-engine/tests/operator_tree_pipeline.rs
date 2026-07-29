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

use uqa_core::{Edge, Value, Vertex};
use uqa_engine::operator_tree_bridge::EngineDriver;
use uqa_engine::Engine;
use uqa_operators::{OperatorTree, TextScoringMode};
use uqa_planner::executor::OperatorTreeDriver;
use uqa_sql::SQLError;

fn engine_with_corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT, year INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX notes_fts_idx ON notes USING gin (title, body)",
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

#[test]
fn driver_propagates_leaf_failure_through_boolean_branches() {
    let eng = engine_with_corpus();
    let driver = EngineDriver::new(&eng, "notes", &[]);
    let tree = OperatorTree::Union(vec![
        OperatorTree::Term {
            query: "tokio".into(),
            field: Some("missing".into()),
            scoring: Some(TextScoringMode::BM25),
        },
        OperatorTree::Empty,
    ]);

    match driver.execute_node(&tree) {
        Err(SQLError::TypeMismatch(message)) => {
            assert!(message.contains("missing"), "unexpected error: {message}");
        }
        other => panic!("expected the search helper error, got {other:?}"),
    }
}

#[test]
fn graph_ir_node_executes_through_the_shared_driver() {
    let eng = engine_with_corpus();
    eng.create_graph("social");
    eng.add_graph_vertex(Vertex::new(1, "Person"), "social");
    eng.add_graph_vertex(Vertex::new(2, "Person"), "social");
    eng.add_graph_edge(Edge::new(1, 1, 2, "follows"), "social");
    let driver = EngineDriver::new(&eng, "notes", &[]);

    let result = driver
        .execute_node(&OperatorTree::PageRank {
            graph: "social".into(),
        })
        .expect("PageRank must execute through EngineDriver");
    assert_eq!(
        result
            .as_posting()
            .expect("PageRank produces one posting per vertex")
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}
