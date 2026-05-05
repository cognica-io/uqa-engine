//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relevance benchmark (BEIR fixture v2): replays the fixture and times
//! the end-to-end retrieval loop for every declared scorer while
//! asserting NDCG@K and MAP@K stay at or above the configured floor.
//! A regression in either dimension (latency or ranking quality) shows
//! up in `cargo bench` output.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
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
    #[allow(dead_code)]
    id: String,
    text: String,
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
        "fixture schema version mismatch"
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

fn build_engine(fx: &Fixture) -> Engine {
    let engine = Engine::new();
    engine.create_default_table("docs", vec![fx.field.clone()]);
    for c in &fx.corpus {
        let mut d = Document::new();
        d.insert(fx.field.clone(), Value::Str(c.body.clone()));
        engine.add_document("docs", c.id, d);
    }
    engine
}

fn measure_relevance(engine: &Engine, fx: &Fixture, mode: &ScoringMode) -> (f64, f64) {
    let mut ndcg_sum = 0.0;
    let mut map_sum = 0.0;
    let mut q_count: u32 = 0;
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
        ndcg_sum += ndcg_at_k(&relevances, fx.k);
        let relevant_ids: BTreeSet<u64> = judgments
            .iter()
            .filter(|(_, rel)| **rel > 0.0)
            .map(|(id, _)| *id)
            .collect();
        let is_relevant: Vec<bool> = hits
            .iter()
            .map(|h| relevant_ids.contains(&h.doc_id))
            .collect();
        map_sum += average_precision_at_k(&is_relevant, relevant_ids.len(), fx.k);
        q_count += 1;
    }
    let n = f64::from(q_count.max(1));
    (ndcg_sum / n, map_sum / n)
}

fn bench_relevance(c: &mut Criterion) {
    let fx = load_fixture();
    let engine = build_engine(&fx);

    for scorer in &fx.scorers {
        let mode = parse_mode(&scorer.name);
        let (mean_ndcg, mean_map) = measure_relevance(&engine, &fx, &mode);
        eprintln!(
            "[relevance bench :: {}] mean NDCG@{} = {mean_ndcg:.4} (floor {}); mean MAP@{} = {mean_map:.4} (floor {})",
            scorer.name, fx.k, scorer.min_ndcg, fx.k, scorer.min_map,
        );
        assert!(
            mean_ndcg >= scorer.min_ndcg,
            "[{}] mean NDCG@{} = {mean_ndcg:.4} below floor {}",
            scorer.name,
            fx.k,
            scorer.min_ndcg,
        );
        assert!(
            mean_map >= scorer.min_map,
            "[{}] mean MAP@{} = {mean_map:.4} below floor {}",
            scorer.name,
            fx.k,
            scorer.min_map,
        );

        let label = format!(
            "beir_fixture_{}_queries_at_k{}_{}",
            fx.queries.len(),
            fx.k,
            scorer.name,
        );
        c.bench_function(&label, |b| {
            b.iter(|| {
                for q in &fx.queries {
                    let hits = engine.search(
                        black_box("docs"),
                        black_box(&fx.field),
                        black_box(&q.text),
                        &mode,
                        fx.k,
                    );
                    black_box(hits);
                }
            });
        });
    }
}

criterion_group!(benches, bench_relevance);
criterion_main!(benches);
