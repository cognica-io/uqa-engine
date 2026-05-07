//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of SQL portions of Python `test_multi_stage.py`.

use uqa_engine::Engine;

fn engine() -> Engine {
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
            "INSERT INTO docs (content) VALUES \
             ('machine learning algorithms'), \
             ('deep learning neural networks'), \
             ('database indexing structures'), \
             ('search engine optimization')",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn test_staged_retrieval_sql() {
    let result = engine()
        .sql(
            "SELECT * FROM docs WHERE staged_retrieval(\
             bayesian_match(content, 'learning'), 3, \
             bayesian_match(content, 'algorithms'), 1)",
            &[],
        )
        .unwrap();
    assert!(result.rows.len() <= 1);
}

#[test]
fn test_staged_retrieval_single_stage() {
    let result = engine()
        .sql(
            "SELECT * FROM docs WHERE staged_retrieval(\
             bayesian_match(content, 'learning'), 2)",
            &[],
        )
        .unwrap();
    assert!(result.rows.len() <= 2);
}
