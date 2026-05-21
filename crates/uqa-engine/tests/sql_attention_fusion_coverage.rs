//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL coverage for `test_attention_fusion`.

use uqa_core::Value;
use uqa_engine::Engine;

fn engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (title TEXT, body TEXT, status TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX idx_docs_gin ON docs USING gin (title, body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (title, body, status) VALUES \
             ('machine learning basics', 'deep neural networks for classification', 'indexed'), \
             ('database systems intro', 'query optimization techniques overview', 'draft'), \
             ('information retrieval', 'search engine ranking algorithms today', 'indexed')",
            &[],
        )
        .unwrap();
    engine
}

fn assert_nonempty_unit_scores(result: &uqa_sql::SQLResult) {
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        match row.get("_score") {
            Some(Value::Float(score)) => assert!(*score > 0.0 && *score < 1.0),
            other => panic!("missing float _score: {other:?}"),
        }
    }
}

#[test]
fn test_fuse_attention_sql() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_attention(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural')) ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert_nonempty_unit_scores(&result);
}

#[test]
fn fuse_attention_combines_with_relational_filter() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_attention(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural')) \
             AND status = 'indexed' ORDER BY title",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_nonempty_unit_scores(&result);
}

#[test]
fn test_fuse_attention_normalize_sql() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_attention(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural'), \
             normalized => true, alpha => 0.5) ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert_nonempty_unit_scores(&result);
}

#[test]
fn test_fuse_attention_with_base_rate() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_attention(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural'), \
             base_rate => 0.01) ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert_nonempty_unit_scores(&result);
}

#[test]
fn test_fuse_multihead_sql() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_multihead(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural'), \
             n_heads => 4, normalized => true) ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert_nonempty_unit_scores(&result);
}

#[test]
fn test_fuse_multihead_default_heads() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_multihead(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural')) ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert_nonempty_unit_scores(&result);
}

#[test]
fn test_fuse_learned_with_alpha() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_learned(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural'), \
             alpha => 0.7) ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert_nonempty_unit_scores(&result);
}

#[test]
fn fuse_learned_combines_with_relational_filter() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_learned(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural')) \
             AND status = 'indexed' ORDER BY title",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_nonempty_unit_scores(&result);
}

#[test]
fn test_fuse_learned_sql() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_learned(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural')) ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert_nonempty_unit_scores(&result);
}
