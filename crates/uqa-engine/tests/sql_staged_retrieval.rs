//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for the cascading `staged_retrieval` SQL function.

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
            "CREATE INDEX docs_fts_idx ON docs USING gin (title, body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (id, title, body) VALUES \
             (1, 'rust async story', 'futures tokio and async io in rust'), \
             (2, 'python web frameworks', 'flask django and python tooling'), \
             (3, 'rust language guide', 'a deep dive into rust generics'), \
             (4, 'go concurrency', 'channels and goroutines for go programs'), \
             (5, 'rust embedded systems', 'rust on no_std targets and async drivers')",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn staged_retrieval_filters_each_subsequent_stage_to_prior_set() {
    let engine = engine_with_corpus();
    // Stage 1: title contains "rust" — top 4. Stage 2 from that pool:
    // body contains "async" — top 5. Only docs 1 and 5 satisfy both.
    let result = engine
        .sql(
            "SELECT id FROM docs \
             WHERE staged_retrieval(title, 'rust', 4, body, 'async', 5) \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&5));
    assert!(!ids.contains(&3));
    assert!(!ids.contains(&2));
    assert!(!ids.contains(&4));
}

#[test]
fn staged_retrieval_arity_must_be_multiple_of_three() {
    let engine = engine_with_corpus();
    let err = engine
        .sql(
            "SELECT id FROM docs WHERE staged_retrieval(title, 'rust', 4, body)",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("staged_retrieval"), "{msg}");
}

#[test]
fn staged_retrieval_top_k_caps_per_stage() {
    let engine = engine_with_corpus();
    // Top 1 in stage 1 by title:rust. Stage 2: top 1 by body:async on
    // that single doc; if it doesn't match, the result is empty.
    let result = engine
        .sql(
            "SELECT id FROM docs \
             WHERE staged_retrieval(title, 'rust', 1, body, 'async', 1)",
            &[],
        )
        .unwrap();
    assert!(result.rows.len() <= 1);
}
