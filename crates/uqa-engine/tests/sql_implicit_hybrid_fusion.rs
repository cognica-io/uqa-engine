//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end SQL coverage for automatic text and vector fusion.

use uqa_core::Value;
use uqa_engine::{Engine, HybridSearchParams, RobustHybridSearchParams, SQLParam};

fn populate(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE docs (\
                 id INTEGER PRIMARY KEY, \
                 body TEXT, \
                 embedding VECTOR(2), \
                 kind TEXT\
             )",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (id, body, embedding, kind) VALUES \
             (1, 'alpha text', ARRAY[0.0, 1.0], 'keep'), \
             (2, 'beta text', ARRAY[1.0, 0.0], 'keep'), \
             (3, 'alpha notes', ARRAY[0.8, 0.2], 'drop')",
            &[],
        )
        .unwrap();
}

fn engine() -> Engine {
    let engine = Engine::new();
    populate(&engine);
    engine
}

fn ids(result: &uqa_sql::SQLResult) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(id)) => *id,
            other => panic!("integer id expected, got {other:?}"),
        })
        .collect()
}

fn scored_rows(result: &uqa_sql::SQLResult) -> Vec<(u64, f64)> {
    result
        .rows
        .iter()
        .map(|row| {
            let id = match row.get("id") {
                Some(Value::Int(id)) => *id as u64,
                other => panic!("integer id expected, got {other:?}"),
            };
            let score = match row.get("_score") {
                Some(Value::Float(score)) => *score,
                other => panic!("floating _score expected, got {other:?}"),
            };
            (id, score)
        })
        .collect()
}

#[test]
fn implicit_hybrid_fusion_matches_canonical_explicit_log_odds() {
    let engine = engine();
    let implicit = engine
        .sql(
            "SELECT id, _score FROM docs \
             WHERE text_match(body, 'alpha') \
               AND knn_match(embedding, ARRAY[1.0, 0.0], 1) \
             ORDER BY _score DESC, id ASC",
            &[],
        )
        .unwrap();
    let explicit = engine
        .sql(
            "SELECT id, _score FROM docs \
             WHERE fuse_bayesian_evidence(\
                 bayesian_match(body, 'alpha'), \
                 knn_match(embedding, ARRAY[1.0, 0.0], 1)\
             ) \
             ORDER BY _score DESC, id ASC",
            &[],
        )
        .unwrap();
    let exact_alias = engine
        .sql(
            "SELECT id, _score FROM docs \
             WHERE fuse_log_odds(\
                 bayesian_match(body, 'alpha'), \
                 knn_match(embedding, ARRAY[1.0, 0.0], 1)\
             ) \
             ORDER BY _score DESC, id ASC",
            &[],
        )
        .unwrap();

    assert_eq!(implicit.columns, explicit.columns);
    assert_eq!(implicit.rows, explicit.rows);
    assert_eq!(exact_alias.rows, explicit.rows);
    let mut actual_ids = ids(&implicit);
    actual_ids.sort_unstable();
    assert_eq!(actual_ids, vec![1, 2, 3]);
}

#[test]
fn typed_hybrid_api_uses_exact_fusion_and_robust_api_remains_distinct() {
    let engine = engine();
    let exact = engine
        .hybrid_search(&HybridSearchParams {
            table: "docs",
            text_field: "body",
            text_query: "alpha",
            vector_field: "embedding",
            query_vector: vec![1.0, 0.0],
            knn_pool: 1,
            top_k: 10,
        })
        .unwrap();
    let explicit = engine
        .sql(
            "SELECT id, _score FROM docs \
             WHERE fuse_bayesian_evidence(\
                 bayesian_match(body, 'alpha'), \
                 knn_match(embedding, ARRAY[1.0, 0.0], 1)\
             ) \
             ORDER BY _score DESC, id ASC",
            &[],
        )
        .unwrap();
    let explicit = scored_rows(&explicit);
    assert_eq!(exact.len(), explicit.len());
    for (actual, (expected_id, expected_score)) in exact.iter().zip(explicit) {
        assert_eq!(actual.doc_id, expected_id);
        assert!((actual.score - expected_score).abs() < 1e-12);
    }

    let robust = engine
        .robust_hybrid_search(&RobustHybridSearchParams {
            table: "docs",
            text_field: "body",
            text_query: "alpha",
            vector_field: "embedding",
            query_vector: vec![1.0, 0.0],
            knn_pool: 1,
            alpha: 0.5,
            top_k: 10,
        })
        .unwrap();
    assert!(
        exact.iter().zip(&robust).any(|(left, right)| {
            left.doc_id != right.doc_id || (left.score - right.score).abs() > 1e-9
        }),
        "exact and robust APIs must not silently share one scoring contract"
    );
}

#[test]
fn implicit_hybrid_fusion_binds_parameters_and_applies_relational_filters() {
    let engine = engine();
    let params = [
        SQLParam::scalar(Value::Str("alpha".into())),
        SQLParam::vector(vec![1.0, 0.0]),
    ];
    let implicit = engine
        .sql(
            "SELECT id, _score FROM docs \
             WHERE text_match(body, $1) \
               AND knn_match(embedding, $2, 1) \
               AND kind = 'keep' \
             ORDER BY _score DESC, id ASC",
            &params,
        )
        .unwrap();
    let explicit = engine
        .sql(
            "SELECT id, _score FROM docs \
             WHERE fuse_bayesian_evidence(\
                 bayesian_match(body, $1), \
                 knn_match(embedding, $2, 1)\
             ) AND kind = 'keep' \
             ORDER BY _score DESC, id ASC",
            &params,
        )
        .unwrap();

    assert_eq!(implicit.columns, explicit.columns);
    assert_eq!(implicit.rows, explicit.rows);
    let mut actual_ids = ids(&implicit);
    actual_ids.sort_unstable();
    assert_eq!(actual_ids, vec![1, 2]);
}

#[test]
fn persistent_implicit_hybrid_fusion_commits_and_restores_calibration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("implicit-hybrid.sqlite3");
    let query = "SELECT id, _score FROM docs \
                 WHERE text_match(body, 'alpha') \
                   AND knn_match(embedding, ARRAY[1.0, 0.0], 1) \
                 ORDER BY _score DESC, id ASC";
    let first_rows = {
        let engine = Engine::open(&path).unwrap();
        populate(&engine);
        engine.sql(query, &[]).unwrap().rows
    };
    let reopened = Engine::open(&path).unwrap();
    let restored_rows = reopened.sql(query, &[]).unwrap().rows;
    assert_eq!(restored_rows, first_rows);
}

#[test]
fn exact_fusion_rejects_conflicting_implicit_priors() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE articles (id INTEGER PRIMARY KEY, body TEXT, title TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX articles_body_gin ON articles USING gin (body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX articles_title_gin ON articles USING gin (title)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO articles (id, body, title) VALUES \
             (1, 'alpha body', 'alpha title'), \
             (2, 'alpha notes', 'beta title')",
            &[],
        )
        .unwrap();
    engine
        .save_scoring_params(
            "articles.body",
            r#"{"alpha":1.0,"beta":0.0,"base_rate":0.1}"#,
        )
        .unwrap();
    engine
        .save_scoring_params(
            "articles.title",
            r#"{"alpha":1.0,"beta":0.0,"base_rate":0.2}"#,
        )
        .unwrap();

    let error = engine
        .sql(
            "SELECT id FROM articles WHERE fuse_bayesian_evidence(\
                 bayesian_match(body, 'alpha'), \
                 bayesian_match(title, 'alpha')\
             )",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("requires one corpus prior"), "{error}");

    let explicit = engine
        .sql(
            "SELECT id, _score FROM articles WHERE fuse_bayesian_evidence(\
                 bayesian_match(body, 'alpha'), \
                 bayesian_match(title, 'alpha'), \
                 base_rate => 0.15\
             ) ORDER BY _score DESC, id ASC",
            &[],
        )
        .unwrap();
    assert!(!explicit.rows.is_empty());
}
