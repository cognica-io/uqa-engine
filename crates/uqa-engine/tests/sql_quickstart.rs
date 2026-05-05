//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Drives the Phase 5 quickstart slice through `Engine::sql`. Mirrors
//! the upstream Python `examples/quickstart.py` flow (CREATE TABLE,
//! INSERT, text/vector/hybrid SELECT) and asserts the dispatched
//! ranking matches `Engine::search` / `Engine::knn_search` /
//! `Engine::hybrid_search` directly.

use std::collections::BTreeMap;

use uqa_core::{FieldName, Value};
use uqa_engine::{Engine, HybridSearchParams, SQLParam, ScoringMode};
use uqa_scoring::BayesianBM25Params;
use uqa_storage::document_store::Document;

fn setup() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, body TEXT, embedding VECTOR(4))",
        &[],
    )
    .unwrap();
    eng.sql("CREATE INDEX idx_docs_gin ON docs USING gin (body)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO docs (id, title, body, embedding) VALUES \
         (1, 'Introduction to UQA', \
          'UQA unifies relational text vector and graph queries through posting lists', \
          ARRAY[0.9, 0.1, 0.0, 0.0]), \
         (2, 'Vector Search Basics', \
          'vector similarity search finds nearest neighbors in embedding space', \
          ARRAY[0.1, 0.9, 0.1, 0.0]), \
         (3, 'Graph Databases', \
          'graph queries traverse vertices and edges to discover relationships', \
          ARRAY[0.0, 0.1, 0.9, 0.0]), \
         (4, 'Hybrid Retrieval', \
          'combining text search with vector similarity improves retrieval quality', \
          ARRAY[0.5, 0.5, 0.0, 0.1])",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn text_match_select_matches_engine_search() {
    let eng = setup();
    let result = eng
        .sql(
            "SELECT id, title, _score AS s FROM docs \
             WHERE text_match(body, 'vector search') ORDER BY s DESC",
            &[],
        )
        .unwrap();

    assert_eq!(result.columns, vec!["id", "title", "s"]);
    assert!(!result.rows.is_empty());

    // Compare against the engine's direct search API.
    let expected = eng.search(
        "docs",
        "body",
        "vector search",
        &ScoringMode::BayesianBM25(BayesianBM25Params::default()),
        usize::MAX,
    );
    assert_eq!(result.rows.len(), expected.len());
    for (row, exp) in result.rows.iter().zip(&expected) {
        assert_eq!(row.get("id"), Some(&Value::Int(exp.doc_id as i64)));
        match row.get("s") {
            Some(Value::Float(s)) => assert!((s - exp.score).abs() < 1e-12),
            other => panic!("expected Float score, got {other:?}"),
        }
    }
}

#[test]
fn knn_match_select_matches_engine_knn_search() {
    let eng = setup();
    let result = eng
        .sql(
            "SELECT id, _score AS s FROM docs \
             WHERE knn_match(embedding, $1, 3) ORDER BY s DESC",
            &[SQLParam::vector(vec![0.5, 0.5, 0.0, 0.0])],
        )
        .unwrap();

    let expected = eng.knn_search("docs", "embedding", vec![0.5, 0.5, 0.0, 0.0], 3);
    assert_eq!(result.rows.len(), expected.len());
    for (row, exp) in result.rows.iter().zip(&expected) {
        assert_eq!(row.get("id"), Some(&Value::Int(exp.doc_id as i64)));
        match row.get("s") {
            Some(Value::Float(s)) => assert!((s - exp.score).abs() < 1e-12),
            other => panic!("expected Float score, got {other:?}"),
        }
    }
}

#[test]
fn fuse_log_odds_select_matches_engine_hybrid_search() {
    let eng = setup();
    let qvec = vec![0.5, 0.5, 0.0, 0.0];
    let result = eng
        .sql(
            "SELECT id, _score AS s FROM docs \
             WHERE fuse_log_odds( \
                 text_match(body, 'vector search'), \
                 knn_match(embedding, $1, 3) \
             ) ORDER BY s DESC",
            &[SQLParam::vector(qvec.clone())],
        )
        .unwrap();

    let expected = eng.hybrid_search(&HybridSearchParams {
        table: "docs",
        text_field: "body",
        text_query: "vector search",
        vector_field: "embedding",
        query_vector: qvec,
        knn_pool: 3,
        alpha: 0.5,
        top_k: usize::MAX,
    });
    assert_eq!(result.rows.len(), expected.len());
    for (row, exp) in result.rows.iter().zip(&expected) {
        assert_eq!(row.get("id"), Some(&Value::Int(exp.doc_id as i64)));
        match row.get("s") {
            Some(Value::Float(s)) => assert!((s - exp.score).abs() < 1e-12),
            other => panic!("expected Float score, got {other:?}"),
        }
    }
}

#[test]
fn create_table_registers_text_and_vector_fields() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE t (id INTEGER, title TEXT, embedding VECTOR(2))",
        &[],
    )
    .unwrap();
    let mut d = Document::new();
    d.insert("id".into(), Value::Int(1));
    d.insert("title".into(), Value::Str("rust".into()));
    let mut vectors: BTreeMap<FieldName, Vec<f32>> = BTreeMap::new();
    vectors.insert("embedding".into(), vec![1.0, 0.0]);
    eng.add_document_with_vectors("t", 1, d, vectors);

    let hits = eng.knn_search("t", "embedding", vec![1.0, 0.0], 1);
    assert_eq!(hits.first().map(|h| h.doc_id), Some(1));
}
