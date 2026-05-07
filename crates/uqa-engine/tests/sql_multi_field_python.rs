//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-call-shape port from `TestMultiFieldSQL` in `test_multi_field.py`.

use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn setup() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE docs (id SERIAL PRIMARY KEY, title TEXT, body TEXT)",
    );
    exec(
        &engine,
        "CREATE INDEX idx_docs_gin ON docs USING gin (title, body)",
    );
    exec(
        &engine,
        "INSERT INTO docs (title, body) VALUES ('machine learning', 'algorithms for ML')",
    );
    exec(
        &engine,
        "INSERT INTO docs (title, body) VALUES ('cooking recipes', 'pasta and pizza')",
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
fn multi_field_match_too_few_args() {
    let engine = setup();
    assert!(engine
        .sql(
            "SELECT * FROM docs WHERE multi_field_match(title, 'learning')",
            &[],
        )
        .is_err());
}
