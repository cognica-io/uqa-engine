//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for Engine parameter-learning cases in `test_parameter_learning`.

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
             ('database indexing structures'),
             ('search engine optimization')",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn learn_returns_params() {
    let engine = engine_with_docs();
    let result = engine
        .learn_scoring_params("docs", "content", "learning", &[1, 1, 0, 0])
        .unwrap();
    assert!(result.contains_key("alpha"));
    assert!(result.contains_key("beta"));
    assert!(result.contains_key("base_rate"));
}

#[test]
fn learn_wrong_label_count() {
    let engine = engine_with_docs();
    let err = engine
        .learn_scoring_params("docs", "content", "learning", &[1, 0])
        .unwrap_err()
        .to_string();
    assert!(err.contains("labels length"));
}

#[test]
fn learn_nonexistent_table() {
    let engine = engine_with_docs();
    let err = engine
        .learn_scoring_params("nonexistent", "content", "learning", &[1])
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown table") || err.contains("does not exist"));
}

#[test]
fn update_scoring_params() {
    let engine = engine_with_docs();
    engine
        .update_scoring_params("docs", "content", 0.8, 1)
        .unwrap();
}
