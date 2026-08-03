//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attention-fusion SQL coverage.

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

fn scores(result: &uqa_sql::SQLResult) -> Vec<f64> {
    result
        .rows
        .iter()
        .map(|row| match row.get("_score") {
            Some(Value::Float(score)) => *score,
            other => panic!("missing float _score: {other:?}"),
        })
        .collect()
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
    let normalized = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_attention(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'query'), \
             normalized => true, alpha => 0.5) ORDER BY title",
            &[],
        )
        .unwrap();
    let unnormalized = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_attention(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'query'), \
             normalized => false, alpha => 0.5) ORDER BY title",
            &[],
        )
        .unwrap();
    assert_nonempty_unit_scores(&normalized);
    assert_eq!(normalized.rows.len(), unnormalized.rows.len());
    assert!(
        scores(&normalized)
            .iter()
            .zip(scores(&unnormalized))
            .any(|(left, right)| (left - right).abs() > 1e-9),
        "normalized option was ignored by physical execution"
    );
}

#[test]
fn test_fuse_attention_with_base_rate() {
    let engine = engine();
    let with_prior = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_attention(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural'), \
             base_rate => 0.01) ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    let without_prior = engine
        .sql(
            "SELECT title, _score FROM docs WHERE fuse_attention(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural')) ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert_nonempty_unit_scores(&with_prior);
    assert_eq!(with_prior.rows.len(), without_prior.rows.len());
    assert!(
        scores(&with_prior)
            .iter()
            .zip(scores(&without_prior))
            .all(|(with_prior, without_prior)| *with_prior < without_prior),
        "base_rate prior was ignored or applied outside log-odds fusion"
    );
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
fn attention_options_reject_unknown_duplicate_and_invalid_values() {
    let engine = engine();
    let cases = [
        (
            "temperature => 1.0",
            "unknown option `temperature` for fuse_attention",
        ),
        (
            "normalized => true, normalized => false",
            "duplicate option `normalized` for fuse_attention",
        ),
        ("base_rate => 0.0", "base_rate must be finite and in (0, 1)"),
        ("normalized => 1", "normalized must be a constant boolean"),
    ];
    for (options, expected) in cases {
        let sql = format!(
            "SELECT title FROM docs WHERE fuse_attention(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural'), {options})"
        );
        let error = engine.sql(&sql, &[]).expect_err("invalid option must fail");
        assert!(error.to_string().contains(expected), "{error}");
    }

    let cases = [
        ("n_heads => 0", "n_heads must be greater than zero"),
        (
            "base_rate => 0.1",
            "unknown option `base_rate` for fuse_multihead",
        ),
        (
            "n_heads => 2, n_heads => 3",
            "duplicate option `n_heads` for fuse_multihead",
        ),
    ];
    for (options, expected) in cases {
        let sql = format!(
            "SELECT title FROM docs WHERE fuse_multihead(\
             bayesian_match(title, 'machine'), \
             bayesian_match(body, 'neural'), {options})"
        );
        let error = engine.sql(&sql, &[]).expect_err("invalid option must fail");
        assert!(error.to_string().contains(expected), "{error}");
    }
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
