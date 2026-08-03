//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! External-prior SQL coverage.

use uqa_core::Value;
use uqa_engine::Engine;

fn engine_with_docs() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id SERIAL PRIMARY KEY, content TEXT, authority TEXT, status TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX idx_docs_gin ON docs USING gin (content)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (content, authority, status) VALUES
             ('machine learning', 'high', 'indexed'),
             ('deep learning', 'low', 'draft')",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn bayesian_with_prior_sql() {
    let engine = engine_with_docs();
    let result = engine
        .sql(
            "SELECT * FROM docs WHERE
             bayesian_match_with_prior(content, 'learning', authority, 'authority')",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn bayesian_with_prior_combines_with_relational_filter() {
    let engine = engine_with_docs();
    let result = engine
        .sql(
            "SELECT id FROM docs WHERE
             bayesian_match_with_prior(content, 'learning', authority, 'authority')
             AND status = 'indexed'
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
fn sparse_threshold_with_prior_combines_with_relational_filter() {
    let engine = engine_with_docs();
    let result = engine
        .sql(
            "SELECT id FROM docs WHERE
             sparse_threshold(
                 bayesian_match_with_prior(content, 'learning', authority, 'authority'),
                 0.01
             )
             AND status = 'indexed'
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
fn bayesian_with_prior_in_fuse_attention() {
    let engine = engine_with_docs();
    let result = engine
        .sql(
            "SELECT content, _score FROM docs WHERE fuse_attention(
                bayesian_match_with_prior(content, 'learning', authority, 'authority'),
                bayesian_match(content, 'machine')
             ) ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in result.rows {
        let Some(Value::Float(score)) = row.get("_score") else {
            panic!("missing _score");
        };
        assert!(*score > 0.0);
        assert!(*score < 1.0);
    }
}

#[test]
fn bayesian_with_prior_invalid_mode() {
    let engine = engine_with_docs();
    let err = engine
        .sql(
            "SELECT * FROM docs WHERE
             bayesian_match_with_prior(content, 'learning', authority, 'invalid')",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("Unknown prior mode"));
}
