//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! BEIR-style fixture-driven relevance gate (v2 schema).
//!
//! Loads a JSON fixture with a corpus, query set, graded judgments,
//! and a `scorers` array. Each entry in `scorers` names a scoring mode
//! (`bm25` or `bayesian_bm25`) plus floor values for `NDCG@K` and
//! `MAP@K`. The harness scores every query under every declared scorer
//! and asserts each scorer's floors hold. Shipping a real BEIR
//! dataset is an out-of-band concern; the loader makes plugging one
//! in a matter of replacing the fixture file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use uqa_core::Value;
use uqa_engine::{Engine, ScoringMode};
use uqa_scoring::{average_precision_at_k, ndcg_at_k, BM25Params, BayesianBM25Params};
use uqa_storage::document_store::Document;

const SCHEMA_VERSION: u32 = 2;

#[derive(Deserialize)]
struct Fixture {
    version: u32,
    field: String,
    k: usize,
    scorers: Vec<ScorerSpec>,
    corpus: Vec<CorpusDoc>,
    queries: Vec<QueryCase>,
}

#[derive(Deserialize)]
struct ScorerSpec {
    name: String,
    min_ndcg: f64,
    min_map: f64,
}

#[derive(Deserialize)]
struct CorpusDoc {
    id: u64,
    body: String,
}

#[derive(Deserialize)]
struct QueryCase {
    id: String,
    text: String,
    /// `doc_id` (as a JSON string key) -> graded relevance. JSON object
    /// keys are strings; the harness parses them back into `u64`.
    judgments: BTreeMap<String, f64>,
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
        .join("beir_fixture.json")
}

fn load_fixture() -> Fixture {
    let bytes = std::fs::read(fixture_path()).expect("fixture present");
    let fx: Fixture = serde_json::from_slice(&bytes).expect("fixture parses");
    assert_eq!(
        fx.version, SCHEMA_VERSION,
        "BEIR fixture schema version {} but harness expects {SCHEMA_VERSION}",
        fx.version,
    );
    assert!(
        !fx.scorers.is_empty(),
        "fixture must declare at least one scorer",
    );
    fx
}

fn parse_mode(name: &str) -> ScoringMode {
    match name {
        "bm25" => ScoringMode::BM25(BM25Params::default()),
        "bayesian_bm25" => ScoringMode::BayesianBM25(BayesianBM25Params::default()),
        other => panic!("unknown scoring mode: {other}"),
    }
}

fn engine_for(fx: &Fixture) -> Engine {
    let engine = Engine::new();
    engine.create_default_table("docs", vec![fx.field.clone()]);
    for c in &fx.corpus {
        let mut d = Document::new();
        d.insert(fx.field.clone(), Value::Str(c.body.clone()));
        engine.add_document("docs", c.id, d);
    }
    engine
}

fn evaluate(engine: &Engine, fx: &Fixture, mode: &ScoringMode) -> (Vec<(String, f64)>, f64) {
    let mut per_query_ndcg = Vec::with_capacity(fx.queries.len());
    let mut map_sum = 0.0;
    for q in &fx.queries {
        let hits = engine.search("docs", &fx.field, &q.text, mode, fx.k);
        let judgments: BTreeMap<u64, f64> = q
            .judgments
            .iter()
            .filter_map(|(k, v)| k.parse::<u64>().ok().map(|id| (id, *v)))
            .collect();
        let relevances: Vec<f64> = hits
            .iter()
            .map(|h| judgments.get(&h.doc_id).copied().unwrap_or(0.0))
            .collect();
        let n = ndcg_at_k(&relevances, fx.k);
        per_query_ndcg.push((q.id.clone(), n));
        let relevant_ids: std::collections::BTreeSet<u64> = judgments
            .iter()
            .filter(|(_, rel)| **rel > 0.0)
            .map(|(id, _)| *id)
            .collect();
        let is_relevant: Vec<bool> = hits
            .iter()
            .map(|h| relevant_ids.contains(&h.doc_id))
            .collect();
        map_sum += average_precision_at_k(&is_relevant, relevant_ids.len(), fx.k);
    }
    let q_count = u32::try_from(fx.queries.len()).expect("query count fits in u32");
    let mean_map = if q_count == 0 {
        0.0
    } else {
        map_sum / f64::from(q_count)
    };
    (per_query_ndcg, mean_map)
}

#[test]
fn beir_fixture_clears_floors_for_every_declared_scorer() {
    let fx = load_fixture();
    let engine = engine_for(&fx);

    for scorer in &fx.scorers {
        let mode = parse_mode(&scorer.name);
        let (per_query, mean_map) = evaluate(&engine, &fx, &mode);

        for (qid, n) in &per_query {
            assert!(
                *n >= scorer.min_ndcg,
                "[{}/{qid}] NDCG@{} = {n:.4} below floor {}",
                scorer.name,
                fx.k,
                scorer.min_ndcg,
            );
        }
        assert!(
            mean_map >= scorer.min_map,
            "[{}] mean MAP@{} = {mean_map:.4} below floor {}",
            scorer.name,
            fx.k,
            scorer.min_map,
        );
    }
}
