//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! BEIR-style relevance gate: score a small graded-judgment corpus
//! through both BM25 and the query-level Bayesian calibration path.
//! The Bayesian transform must preserve the BM25 ranking and NDCG@5,
//! while the shared ranking stays above an empirical quality floor.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_engine::{Engine, ScoringMode};
use uqa_scoring::{ndcg_at_k, BM25Params, BayesianBM25Params};
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
            body: "flask and django and python web tooling",
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
    engine
        .create_default_table("docs", vec!["title".into(), "body".into()])
        .unwrap();
    for doc in corpus() {
        let mut d = Document::new();
        d.insert("title".into(), Value::Str(doc.title.into()));
        d.insert("body".into(), Value::Str(doc.body.into()));
        engine.add_document("docs", doc.id, d).unwrap();
    }
    engine
}

// Empirical floor: anchored to the current Bayesian BM25 baseline
// on this synthetic corpus. The intent is a regression tripwire,
// not a quality target - we set the bar a few points below the
// worst observed NDCG@5 (~0.794 for the bare "rust" query, where
// a partial-match doc ranks above a fully-on-topic one).
const NDCG_K: usize = 5;
const MIN_NDCG: f64 = 0.75;
const MAP_K: usize = 5;
const MIN_MAP: f64 = 0.7;

#[test]
fn bayesian_bm25_preserves_bm25_ranking_and_ndcg() {
    let engine = engine_with_corpus();
    let bm25_mode = ScoringMode::BM25(BM25Params::default());
    let bayesian_mode = ScoringMode::BayesianBM25(BayesianBM25Params::default());
    for q in queries() {
        let bm25_hits = engine
            .search("docs", "body", q.text, &bm25_mode, NDCG_K)
            .unwrap();
        let bayesian_hits = engine
            .search("docs", "body", q.text, &bayesian_mode, NDCG_K)
            .unwrap();
        assert_eq!(
            bm25_hits.iter().map(|hit| hit.doc_id).collect::<Vec<_>>(),
            bayesian_hits
                .iter()
                .map(|hit| hit.doc_id)
                .collect::<Vec<_>>(),
            "query {:?} changed ranking after monotone calibration",
            q.text,
        );

        let bm25_relevances: Vec<f64> = bm25_hits
            .iter()
            .map(|h| q.judgments.get(&h.doc_id).copied().unwrap_or(0.0))
            .collect();
        let bayesian_relevances: Vec<f64> = bayesian_hits
            .iter()
            .map(|h| q.judgments.get(&h.doc_id).copied().unwrap_or(0.0))
            .collect();
        let bm25_ndcg = ndcg_at_k(&bm25_relevances, NDCG_K);
        let bayesian_ndcg = ndcg_at_k(&bayesian_relevances, NDCG_K);
        assert!((bayesian_ndcg - bm25_ndcg).abs() < 1e-12);
        assert!(
            bayesian_ndcg >= MIN_NDCG,
            "query {:?} produced NDCG@{NDCG_K} {bayesian_ndcg:.4} (< {MIN_NDCG}); top hits: {:?}",
            q.text,
            bayesian_hits
                .iter()
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
        let hits = engine.search("docs", "body", q.text, &mode, MAP_K).unwrap();
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
