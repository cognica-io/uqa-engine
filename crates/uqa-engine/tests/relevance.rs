//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! BEIR-style relevance gate: score a small graded-judgment corpus
//! through the engine's Bayesian BM25 path and assert NDCG@5 stays
//! above an empirical floor. This is the smallest possible IR-quality
//! tripwire — full BEIR datasets land in a follow-up benchmark, but
//! this one will catch any regression that flips the ranking on
//! controlled inputs.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_engine::{Engine, ScoringMode};
use uqa_scoring::{ndcg_at_k, BayesianBM25Params};
use uqa_storage::document_store::Document;

#[derive(Clone)]
struct CorpusDoc {
    id: u64,
    title: &'static str,
    body: &'static str,
}

#[derive(Clone)]
struct Query {
    text: &'static str,
    /// Map from `doc_id` to graded relevance (0 = not relevant).
    judgments: BTreeMap<u64, f64>,
}

fn corpus() -> Vec<CorpusDoc> {
    vec![
        CorpusDoc {
            id: 1,
            title: "rust async story",
            body: "futures and tokio runtime in rust",
        },
        CorpusDoc {
            id: 2,
            title: "rust language guide",
            body: "a deep dive into rust generics and traits",
        },
        CorpusDoc {
            id: 3,
            title: "python web frameworks",
            body: "flask and django and python tooling",
        },
        CorpusDoc {
            id: 4,
            title: "rust embedded systems",
            body: "rust on no_std targets and async drivers",
        },
        CorpusDoc {
            id: 5,
            title: "go networking",
            body: "channels and goroutines for go programs",
        },
        CorpusDoc {
            id: 6,
            title: "rust web servers",
            body: "axum hyper and reqwest for rust web servers",
        },
        CorpusDoc {
            id: 7,
            title: "data pipelines",
            body: "etl jobs in python and rust",
        },
    ]
}

fn queries() -> Vec<Query> {
    vec![
        // "rust async": docs 1 (perfect) and 4 (good) are relevant.
        Query {
            text: "rust async",
            judgments: BTreeMap::from([(1, 3.0), (4, 2.0), (2, 1.0)]),
        },
        // "python web": docs 3 (perfect) and 7 (partial) are relevant.
        Query {
            text: "python web",
            judgments: BTreeMap::from([(3, 3.0), (7, 1.0)]),
        },
        // "rust": several rust-tagged docs are relevant.
        Query {
            text: "rust",
            judgments: BTreeMap::from([(1, 2.0), (2, 3.0), (4, 2.0), (6, 2.0), (7, 1.0)]),
        },
    ]
}

fn engine_with_corpus() -> Engine {
    let engine = Engine::new();
    engine.create_default_table("docs", vec!["title".into(), "body".into()]);
    for doc in corpus() {
        let mut d = Document::new();
        d.insert("title".into(), Value::Str(doc.title.into()));
        d.insert("body".into(), Value::Str(doc.body.into()));
        engine.add_document("docs", doc.id, d);
    }
    engine
}

// Empirical floor: anchored to the current Bayesian BM25 baseline
// on this synthetic corpus. The intent is a regression tripwire,
// not a quality target — we set the bar a few points below the
// worst observed NDCG@5 (~0.794 for the bare "rust" query, where
// a partial-match doc ranks above a fully-on-topic one).
const NDCG_K: usize = 5;
const MIN_NDCG: f64 = 0.75;
const MAP_K: usize = 5;
const MIN_MAP: f64 = 0.7;

#[test]
fn bayesian_bm25_clears_ndcg_floor() {
    let engine = engine_with_corpus();
    let mode = ScoringMode::BayesianBM25(BayesianBM25Params::default());
    for q in queries() {
        let hits = engine.search("docs", "body", q.text, &mode, NDCG_K);
        let relevances: Vec<f64> = hits
            .iter()
            .map(|h| q.judgments.get(&h.doc_id).copied().unwrap_or(0.0))
            .collect();
        let n = ndcg_at_k(&relevances, NDCG_K);
        assert!(
            n >= MIN_NDCG,
            "query {:?} produced NDCG@{NDCG_K} {n:.4} (< {MIN_NDCG}); top hits: {:?}",
            q.text,
            hits.iter()
                .take(NDCG_K)
                .map(|h| h.doc_id)
                .collect::<Vec<_>>(),
        );
    }
}

#[test]
fn map_clears_floor() {
    let engine = engine_with_corpus();
    let mode = ScoringMode::BayesianBM25(BayesianBM25Params::default());
    let mut sum = 0.0;
    let mut n: i32 = 0;
    for q in queries() {
        let hits = engine.search("docs", "body", q.text, &mode, MAP_K);
        let relevant_ids: std::collections::BTreeSet<u64> = q
            .judgments
            .iter()
            .filter(|(_, rel)| **rel > 0.0)
            .map(|(id, _)| *id)
            .collect();
        let total_relevant = relevant_ids.len();
        let is_relevant: Vec<bool> = hits
            .iter()
            .map(|h| relevant_ids.contains(&h.doc_id))
            .collect();
        sum += uqa_scoring::average_precision_at_k(&is_relevant, total_relevant, MAP_K);
        n += 1;
    }
    let map = if n > 0 { sum / f64::from(n) } else { 0.0 };
    assert!(
        map >= MIN_MAP,
        "MAP@{MAP_K} = {map:.4} below floor {MIN_MAP}",
    );
}
