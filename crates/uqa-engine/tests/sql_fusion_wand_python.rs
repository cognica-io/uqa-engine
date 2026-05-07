//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of SQL portions of Python `test_fusion_wand.py`.

use uqa_core::Value;
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
             ('database indexing structures')",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn test_log_odds_fusion_with_limit() {
    let result = engine()
        .sql(
            "SELECT * FROM docs WHERE \
             fuse_log_odds(bayesian_match(content, 'learning'), \
             bayesian_match(content, 'algorithms')) LIMIT 1",
            &[],
        )
        .unwrap();
    assert!(result.rows.len() <= 1);
}

#[test]
fn test_fusion_result_scores() {
    let result = engine()
        .sql(
            "SELECT content, _score FROM docs WHERE \
             fuse_log_odds(bayesian_match(content, 'learning'), \
             bayesian_match(content, 'algorithms'))",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in result.rows {
        match row.get("_score") {
            Some(Value::Float(score)) => assert!(*score > 0.0 && *score < 1.0),
            other => panic!("missing _score: {other:?}"),
        }
    }
}

#[test]
fn test_log_odds_with_gating_relu() {
    let result = engine()
        .sql(
            "SELECT * FROM docs WHERE \
             fuse_log_odds(bayesian_match(content, 'learning'), \
             bayesian_match(content, 'algorithms'), 0.5, 'relu')",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn test_log_odds_with_gating_swish() {
    let result = engine()
        .sql(
            "SELECT * FROM docs WHERE \
             fuse_log_odds(bayesian_match(content, 'learning'), \
             bayesian_match(content, 'algorithms'), 'swish')",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
}
