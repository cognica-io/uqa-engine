//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sparse-threshold SQL coverage.

use uqa_engine::Engine;

fn engine_with_docs() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id SERIAL PRIMARY KEY, content TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX idx_docs_gin ON docs USING gin (content)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (content) VALUES
             ('machine learning algorithms'),
             ('deep learning neural networks'),
             ('database indexing structures')",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn sparse_threshold_sql() {
    let engine = engine_with_docs();
    let result = engine
        .sql(
            "SELECT * FROM docs WHERE sparse_threshold(bayesian_match(content, 'learning'), 0.3)",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn sparse_threshold_invalid_args() {
    let engine = engine_with_docs();
    let err = engine
        .sql(
            "SELECT * FROM docs WHERE sparse_threshold(bayesian_match(content, 'learning'))",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid argument count") || err.contains("sparse_threshold"));
}
