//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Run the existing text-search and hybrid-search parity fixtures
//! against an `Engine::open`-backed (`SQLite`) engine. The `Memory` and
//! `SQLite` backends must produce identical doc id ordering and matching
//! scores. Also covers crash safety: write, drop the engine, reopen on
//! the same path, verify the state survived.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use tempfile::tempdir;
use uqa_core::{FieldName, Value};
use uqa_engine::{Engine, HybridSearchParams, ScoringMode};
use uqa_scoring::{BM25Params, BayesianBM25Params};
use uqa_storage::document_store::Document;

const TEXT_SCORE_EPSILON: f64 = 1e-9;
const HYBRID_SCORE_EPSILON: f64 = 1e-7;

#[derive(Deserialize)]
struct TextFixture {
    corpus: Vec<TextDoc>,
    queries: Vec<TextQuery>,
}

#[derive(Deserialize)]
struct TextDoc {
    id: u64,
    title: String,
    body: String,
}

#[derive(Deserialize)]
struct TextQuery {
    field: String,
    query: String,
    scoring: String,
    top_k: usize,
    expected: Vec<Hit>,
}

#[derive(Deserialize)]
struct HybridFixture {
    vector_dim: u32,
    corpus: Vec<HybridDoc>,
    queries: Vec<HybridQuery>,
}

#[derive(Deserialize)]
struct HybridDoc {
    id: u64,
    title: String,
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct HybridQuery {
    text_field: String,
    text_query: String,
    query_vector: Vec<f32>,
    knn_pool: usize,
    alpha: f64,
    top_k: usize,
    expected: Vec<Hit>,
}

#[derive(Deserialize)]
struct Hit {
    doc_id: u64,
    score: f64,
}

fn parity_path(name: &str) -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests")
        .join("parity")
        .join(name)
}

fn parse_mode(name: &str) -> ScoringMode {
    match name {
        "bm25" => ScoringMode::BM25(BM25Params::default()),
        "bayesian_bm25" => ScoringMode::BayesianBM25(BayesianBM25Params::default()),
        other => panic!("unknown scoring mode in fixture: {other}"),
    }
}

#[test]
fn sqlite_engine_matches_text_search_fixture() {
    let bytes = std::fs::read(parity_path("text_search_fixture.json")).unwrap();
    let fx: TextFixture = serde_json::from_slice(&bytes).unwrap();

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("uqa.sqlite3");
    let eng = Engine::open(&db_path).unwrap();
    eng.create_default_table("articles", vec!["title".into(), "body".into()])
        .unwrap();

    for c in &fx.corpus {
        let mut d = Document::new();
        d.insert("title".into(), Value::Str(c.title.clone()));
        d.insert("body".into(), Value::Str(c.body.clone()));
        eng.add_document("articles", c.id, d).unwrap();
    }

    for case in &fx.queries {
        let mode = parse_mode(&case.scoring);
        let hits = eng
            .search("articles", &case.field, &case.query, &mode, case.top_k)
            .unwrap();
        assert_eq!(
            hits.len(),
            case.expected.len(),
            "[sqlite text {} `{}` {}] hit count differs",
            case.field,
            case.query,
            case.scoring,
        );
        for (got, exp) in hits.iter().zip(&case.expected) {
            assert_eq!(got.doc_id, exp.doc_id);
            assert!(
                (got.score - exp.score).abs() < TEXT_SCORE_EPSILON,
                "score delta {} too large",
                got.score - exp.score
            );
        }
    }
}

#[test]
fn sqlite_engine_matches_hybrid_search_fixture() {
    let bytes = std::fs::read(parity_path("hybrid_search_fixture.json")).unwrap();
    let fx: HybridFixture = serde_json::from_slice(&bytes).unwrap();

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("uqa.sqlite3");
    let eng = Engine::open(&db_path).unwrap();
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
        assert_eq!(
            hits.len(),
            case.expected.len(),
            "[sqlite hybrid `{}` alpha={}] hit count differs",
            case.text_query,
            case.alpha,
        );
        for (got, exp) in hits.iter().zip(&case.expected) {
            assert_eq!(got.doc_id, exp.doc_id);
            assert!(
                (got.score - exp.score).abs() < HYBRID_SCORE_EPSILON,
                "hybrid score delta {} too large",
                got.score - exp.score
            );
        }
    }
}

#[test]
fn engine_state_survives_close_and_reopen() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("uqa.sqlite3");

    let hybrid_params = || HybridSearchParams {
        table: "articles",
        text_field: "title",
        text_query: "rust",
        vector_field: "embedding",
        query_vector: vec![1.0, 0.0, 0.0],
        knn_pool: 5,
        alpha: 0.5,
        top_k: 5,
    };

    let score_before_close;
    {
        let eng = Engine::open(&db_path).unwrap();
        eng.create_default_table("articles", vec!["title".into()])
            .unwrap();
        eng.create_vector_field("articles", "embedding", 3).unwrap();
        let mut d = Document::new();
        d.insert("title".into(), Value::Str("rust language".into()));
        let mut vectors = BTreeMap::new();
        vectors.insert("embedding".into(), vec![1.0f32, 0.0, 0.0]);
        eng.add_document_with_vectors("articles", 42, d, vectors)
            .unwrap();
        let hits = eng.hybrid_search(&hybrid_params()).unwrap();
        assert_eq!(hits.first().map(|h| h.doc_id), Some(42));
        score_before_close = hits[0].score;
        assert!(score_before_close > 0.0);
    } // engine drops; SQLite connection closes; WAL is checkpointed on next open.

    let eng = Engine::open(&db_path).unwrap();
    let got = eng.get_document("articles", 42).unwrap().unwrap();
    assert_eq!(got.get("title"), Some(&Value::Str("rust language".into())));

    // Hybrid search still works after restore: the inverted index, doc
    // store, vector index, and persisted calibration parameters were
    // restored from the catalog, so the fused probability is identical.
    let hits = eng.hybrid_search(&hybrid_params()).unwrap();
    assert_eq!(hits.first().map(|h| h.doc_id), Some(42));
    assert!(
        (hits[0].score - score_before_close).abs() < 1e-12,
        "hybrid score changed across reopen: {} vs {score_before_close}",
        hits[0].score
    );
}
