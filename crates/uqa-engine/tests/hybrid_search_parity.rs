//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Golden-fixture test: hybrid (text + KNN, log-odds fused) output.
//! Refresh the fixtures with
//! `cargo run -p uqa-engine --example build_parity_fixtures`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use uqa_core::{FieldName, Value};
use uqa_engine::{Engine, HybridSearchParams};
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
    eng.create_default_table("articles", vec!["title".into()]);
    eng.create_vector_field("articles", "embedding", fx.vector_dim);
    for c in &fx.corpus {
        let mut d = Document::new();
        d.insert("title".into(), Value::Str(c.title.clone()));
        let mut vectors: BTreeMap<FieldName, Vec<f32>> = BTreeMap::new();
        vectors.insert("embedding".into(), c.embedding.clone());
        eng.add_document_with_vectors("articles", c.id, d, vectors)
            .unwrap();
    }

    for case in &fx.queries {
        let hits = eng.hybrid_search(&HybridSearchParams {
            table: "articles",
            text_field: &case.text_field,
            text_query: &case.text_query,
            vector_field: "embedding",
            query_vector: case.query_vector.clone(),
            knn_pool: case.knn_pool,
            alpha: case.alpha,
            top_k: case.top_k,
        });
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
