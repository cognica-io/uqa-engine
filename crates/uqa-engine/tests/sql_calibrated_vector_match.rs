//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL coverage for `calibrated_vector_match`.

use uqa_core::Value;
use uqa_engine::Engine;

fn engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, kind TEXT, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (id, kind, embedding) VALUES \
             (1, 'chat', ARRAY[0.9, 0.1]), \
             (2, 'image', ARRAY[0.8, 0.2]), \
             (3, 'chat', ARRAY[0.1, 0.9])",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn calibrated_vector_match_returns_unit_scores() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT id, _score FROM docs \
             WHERE calibrated_vector_match('embedding', ARRAY[0.9, 0.1], 3) \
             ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        let Some(Value::Float(score)) = row.get("_score") else {
            panic!("missing _score: {row:?}");
        };
        assert!(*score > 0.0 && *score < 1.0, "{score}");
    }
}

#[test]
fn calibrated_vector_match_combines_with_relational_filter() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT id, _score FROM docs \
             WHERE calibrated_vector_match('embedding', ARRAY[0.9, 0.1], 3) \
               AND kind = 'chat' \
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
    assert_eq!(ids, vec![1, 3]);
}

#[test]
fn calibrated_vector_match_nests_under_log_odds() {
    let engine = engine();
    engine
        .sql("CREATE INDEX docs_kind_gin ON docs USING gin (kind)", &[])
        .unwrap();
    let result = engine
        .sql(
            "SELECT id, _score FROM docs \
             WHERE fuse_log_odds(\
                 bayesian_match(kind, 'chat'), \
                 calibrated_vector_match('embedding', ARRAY[0.9, 0.1], 3)\
             ) \
             ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn calibrated_vector_match_invalid_threshold_errors() {
    let engine = engine();
    let err = engine
        .sql(
            "SELECT id FROM docs \
             WHERE calibrated_vector_match('embedding', ARRAY[0.9, 0.1], 3, 'bad')",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("threshold"), "{err}");
}
