//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL call-shape coverage for `TestMultiFieldSQL` in `test_multi_field`.

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn setup() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE docs (id SERIAL PRIMARY KEY, title TEXT, body TEXT, status TEXT)",
    );
    exec(
        &engine,
        "CREATE INDEX idx_docs_gin ON docs USING gin (title, body)",
    );
    exec(
        &engine,
        "INSERT INTO docs (title, body, status) VALUES \
         ('machine learning', 'algorithms for ML', 'indexed')",
    );
    exec(
        &engine,
        "INSERT INTO docs (title, body, status) VALUES \
         ('learning recipes', 'pasta and pizza', 'draft')",
    );
    engine
}

#[test]
fn multi_field_match_sql() {
    let engine = setup();
    let result = engine
        .sql(
            "SELECT * FROM docs WHERE multi_field_match(title, body, 'learning')",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn multi_field_match_with_weights() {
    let engine = setup();
    let result = engine
        .sql(
            "SELECT * FROM docs WHERE multi_field_match(title, body, 'learning', 2.0, 0.5)",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn multi_field_match_with_weights_and_relational_filter() {
    let engine = setup();
    let result = engine
        .sql(
            "SELECT id FROM docs \
             WHERE multi_field_match(title, body, 'learning', 2.0, 1.0) \
               AND status = 'indexed' \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(id)) => *id,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1]);
}

#[test]
fn multi_field_match_too_few_args() {
    let engine = setup();
    assert!(engine
        .sql(
            "SELECT * FROM docs WHERE multi_field_match(title, 'learning')",
            &[],
        )
        .is_err());
}
