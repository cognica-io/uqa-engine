//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Golden-fixture test: load text-search expectations
//! and verify that the engine produces the same top-k doc id order
//! and matching BM25 / Bayesian BM25 scores within a small epsilon.
//!
//! Refresh the fixtures with
//! `cargo run -p uqa-engine --example build_parity_fixtures`.

use std::path::PathBuf;

use serde::Deserialize;
use uqa_core::Value;
use uqa_engine::{Engine, ScoringMode};
use uqa_scoring::{BM25Params, BayesianBM25Params};
use uqa_storage::document_store::Document;

const SCORE_EPSILON: f64 = 1e-9;

#[derive(Deserialize)]
struct Fixture {
    #[allow(dead_code)]
    version: u32,
    corpus: Vec<CorpusDoc>,
    queries: Vec<QueryCase>,
}

#[derive(Deserialize)]
struct CorpusDoc {
    id: u64,
    title: String,
    body: String,
}

#[derive(Deserialize)]
struct QueryCase {
    field: String,
    query: String,
    scoring: String,
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
        .join("text_search_fixture.json")
}

fn load_fixture() -> Fixture {
    let bytes = std::fs::read(fixture_path()).expect("fixture present");
    serde_json::from_slice(&bytes).expect("fixture parses")
}

fn into_doc(c: &CorpusDoc) -> Document {
    let mut d = Document::new();
    d.insert("title".into(), Value::Str(c.title.clone()));
    d.insert("body".into(), Value::Str(c.body.clone()));
    d
}

fn parse_mode(name: &str) -> ScoringMode {
    match name {
        "bm25" => ScoringMode::BM25(BM25Params::default()),
        "bayesian_bm25" => ScoringMode::BayesianBM25(BayesianBM25Params::default()),
        other => panic!("unknown scoring mode in fixture: {other}"),
    }
}

#[test]
fn engine_matches_text_search_fixture() {
    let fx = load_fixture();
    let eng = Engine::new();
    eng.create_default_table("articles", vec!["title".into(), "body".into()])
        .unwrap();
    for c in &fx.corpus {
        eng.add_document("articles", c.id, into_doc(c)).unwrap();
    }

    for case in &fx.queries {
        let mode = parse_mode(&case.scoring);
        let hits = eng
            .search("articles", &case.field, &case.query, &mode, case.top_k)
            .unwrap();
        let expected = &case.expected;

        assert_eq!(
            hits.len(),
            expected.len(),
            "[{} `{}` {}] hit count differs: got {}, expected {}",
            case.field,
            case.query,
            case.scoring,
            hits.len(),
            expected.len(),
        );
        for (i, (got, exp)) in hits.iter().zip(expected).enumerate() {
            assert_eq!(
                got.doc_id, exp.doc_id,
                "[{} `{}` {} idx {}] doc_id differs: got {}, expected {}",
                case.field, case.query, case.scoring, i, got.doc_id, exp.doc_id,
            );
            assert!(
                (got.score - exp.score).abs() < SCORE_EPSILON,
                "[{} `{}` {} idx {} doc_id {}] score differs: got {}, expected {} (delta {})",
                case.field,
                case.query,
                case.scoring,
                i,
                got.doc_id,
                got.score,
                exp.score,
                got.score - exp.score,
            );
        }
    }
}
