//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Microbenchmarks for the BM25 and Bayesian BM25 inner-loop
//! scoring functions.
//!
//! Run with `cargo bench -p uqa-scoring --bench bm25`.

use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_core::IndexStats;
use uqa_scoring::{BM25Params, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer};

const N: usize = 100_000;

fn build_corpus() -> Vec<(u64, u64, u64)> {
    // Synthetic per-doc tuples: (term_freq, doc_length, doc_freq).
    // Vary tf 1..20, dl 50..550, df 1..50,000 so the scorer exercises
    // the full BM25 dynamic range.
    (0..N)
        .map(|i| {
            let tf = ((i % 20) + 1) as u64;
            let dl = (50 + (i % 500)) as u64;
            let df = ((i % 50_000) + 1) as u64;
            (tf, dl, df)
        })
        .collect()
}

fn build_stats() -> Arc<IndexStats> {
    let mut s = IndexStats::default();
    s.total_docs = 1_000_000;
    s.avg_doc_length = 200.0;
    Arc::new(s)
}

fn bench_bm25(c: &mut Criterion) {
    let corpus = build_corpus();
    let stats = build_stats();
    let scorer = BM25Scorer::new(BM25Params::default(), stats);
    c.bench_function("bm25_score_100k", |bencher| {
        bencher.iter(|| {
            let mut total = 0.0f64;
            for (tf, dl, df) in &corpus {
                total += scorer.score(*tf, *dl, *df);
            }
            black_box(total)
        });
    });
}

fn bench_bayesian_bm25(c: &mut Criterion) {
    let corpus = build_corpus();
    let stats = build_stats();
    let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), stats).unwrap();
    c.bench_function("bayesian_bm25_score_100k", |bencher| {
        bencher.iter(|| {
            let mut total = 0.0f64;
            for (tf, dl, df) in &corpus {
                total += scorer.score(*tf, *dl, *df);
            }
            black_box(total)
        });
    });
}

criterion_group!(benches, bench_bm25, bench_bayesian_bm25);
criterion_main!(benches);
