//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Golden-fixture test: hybrid (text + KNN, positive-evidence pooled) output.
//! Refresh the fixtures with
//! `cargo run -p uqa-engine --example build_parity_fixtures`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use uqa_core::{FieldName, Value};
use uqa_engine::{Engine, HybridSearchParams};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

// Hybrid pipeline composes BM25 (`f64`) and cosine (`f32` lifted to
// `f64`) so we accept a slightly looser bound than the text-only test.
const SCORE_EPSILON: f64 = 1e-7;

#[derive(Deserialize)]
struct Fixture {
    #[allow(dead_code)]
    version: u32,
    vector_dim: u32,
    corpus: Vec<CorpusDoc>,
    queries: Vec<QueryCase>,
}

#[derive(Deserialize)]
struct CorpusDoc {
    id: u64,
    title: String,
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct QueryCase {
    text_field: String,
    text_query: String,
    query_vector: Vec<f32>,
    knn_pool: usize,
    alpha: f64,
    top_k: usize,
    expected: Vec<ExpectedHit>,
}

#[derive(Deserialize)]
struct ExpectedHit {
    doc_id: u64,
    score: f64,
}

fn fixture_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests")
        .join("parity")
        .join("hybrid_search_fixture.json")
}

#[test]
fn hybrid_search_matches_fixture() {
    let bytes = std::fs::read(fixture_path()).expect("fixture present");
    let fx: Fixture = serde_json::from_slice(&bytes).expect("fixture parses");

    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();
    eng.create_vector_field("articles", "embedding", fx.vector_dim)
        .unwrap();
    for c in &fx.corpus {
        let mut d = Document::new();
        d.insert("title".into(), Value::Str(c.title.clone()));
        let mut vectors: BTreeMap<FieldName, Vec<f32>> = BTreeMap::new();
        vectors.insert("embedding".into(), c.embedding.clone());
        eng.add_document_with_vectors("articles", c.id, d, vectors)
            .unwrap();
    }

    for case in &fx.queries {
        let hits = eng
            .hybrid_search(&HybridSearchParams {
                table: "articles",
                text_field: &case.text_field,
                text_query: &case.text_query,
                vector_field: "embedding",
                query_vector: case.query_vector.clone(),
                knn_pool: case.knn_pool,
                alpha: case.alpha,
                top_k: case.top_k,
            })
            .unwrap();
        let expected = &case.expected;

        assert_eq!(
            hits.len(),
            expected.len(),
            "[`{}` alpha={}] hit count differs: got {}, expected {}",
            case.text_query,
            case.alpha,
            hits.len(),
            expected.len(),
        );
        for (i, (got, exp)) in hits.iter().zip(expected).enumerate() {
            assert_eq!(
                got.doc_id, exp.doc_id,
                "[`{}` alpha={} idx {}] doc_id differs: got {}, expected {}",
                case.text_query, case.alpha, i, got.doc_id, exp.doc_id,
            );
            assert!(
                (got.score - exp.score).abs() < SCORE_EPSILON,
                "[`{}` alpha={} idx {} doc_id {}] score differs: got {}, expected {} (delta {})",
                case.text_query,
                case.alpha,
                i,
                got.doc_id,
                got.score,
                exp.score,
                got.score - exp.score,
            );
        }
    }
}

#[test]
fn hybrid_search_rejects_missing_vector_field_even_without_text_tokens() {
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();

    for text_query in ["rust", ""] {
        let error = eng
            .hybrid_search(&HybridSearchParams {
                table: "articles",
                text_field: "title",
                text_query,
                vector_field: "missing_embedding",
                query_vector: vec![1.0, 0.0, 0.0],
                knn_pool: 10,
                alpha: 0.5,
                top_k: 10,
            })
            .expect_err("a requested missing vector field must not degrade to text-only");
        assert!(
            matches!(error, SQLError::UnknownColumn(ref field) if field == "missing_embedding"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn hybrid_search_validates_vector_shape_before_returning_an_empty_pool() {
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();
    eng.create_vector_field("articles", "embedding", 3).unwrap();

    let error = eng
        .hybrid_search(&HybridSearchParams {
            table: "articles",
            text_field: "title",
            text_query: "",
            vector_field: "embedding",
            query_vector: vec![1.0, 0.0],
            knn_pool: 0,
            alpha: 0.5,
            top_k: 0,
        })
        .expect_err("zero result limits must not bypass vector validation");
    assert!(
        matches!(error, SQLError::TypeMismatch(ref message) if message.contains("has 2 dimensions, expected 3")),
        "unexpected error: {error}"
    );
}

#[test]
fn explicit_scoring_sample_count_cannot_overflow_stride_arithmetic() {
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into()])
        .unwrap();
    let mut document = Document::new();
    document.insert("title".into(), Value::Str("rust systems".into()));
    eng.add_document("articles", 1, document).unwrap();

    eng.estimate_scoring_params("articles", "title", usize::MAX, 1, 7)
        .expect("a huge sample target must saturate stride arithmetic, not overflow");
}
