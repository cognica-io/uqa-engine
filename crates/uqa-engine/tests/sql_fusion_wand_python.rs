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
fn test_log_odds_fusion_with_default_alpha_and_filter() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE messages (\
             id SERIAL PRIMARY KEY, \
             content TEXT, \
             kind TEXT NOT NULL DEFAULT 'chat', \
             embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX idx_messages_gin ON messages USING gin (content)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO messages (content, kind, embedding) VALUES \
             ('machine learning algorithms', 'chat', ARRAY[0.9, 0.1]), \
             ('deep learning neural networks', 'image', ARRAY[0.8, 0.2]), \
             ('database indexing structures', 'chat', ARRAY[0.1, 0.9])",
            &[],
        )
        .unwrap();

    let result = engine
        .sql(
            "SELECT kind, _score FROM messages WHERE \
             fuse_log_odds(\
                 bayesian_match(content, 'learning'), \
                 knn_match(embedding, ARRAY[0.9, 0.1], 3)\
             ) AND kind = 'chat' \
             ORDER BY _score DESC \
             LIMIT 3",
            &[],
        )
        .unwrap();

    assert!(!result.rows.is_empty());
    for row in result.rows {
        assert_eq!(row.get("kind"), Some(&Value::Str("chat".into())));
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
