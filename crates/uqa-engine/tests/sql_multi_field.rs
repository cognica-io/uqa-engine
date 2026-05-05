//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for the `multi_field_match` SQL function.

use uqa_core::Value;
use uqa_engine::Engine;

fn engine_with_corpus() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, body TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (id, title, body) VALUES \
             (1, 'rust language guide', 'a deep dive into rust generics'), \
             (2, 'python web frameworks', 'flask django and python tooling'), \
             (3, 'rust async story', 'futures tokio and async io')",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn multi_field_match_returns_documents_matching_either_field() {
    let engine = engine_with_corpus();
    let result = engine
        .sql(
            "SELECT id, _score \
             FROM docs \
             WHERE multi_field_match(title, 'rust', body, 'rust') \
             ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    // Documents 1 and 3 mention `rust` in both fields and should
    // outrank doc 2 which mentions neither.
    let ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&3));
}

#[test]
fn multi_field_match_arity_must_be_even() {
    let engine = engine_with_corpus();
    let err = engine
        .sql(
            "SELECT id FROM docs WHERE multi_field_match(title, 'rust', body)",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("multi_field_match"), "{msg}");
}
