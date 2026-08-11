//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar scoring and fusion-substrate benchmarks.
//!
//! Covers BM25, Bayesian BM25, vector similarity, probability mapping,
//! and log-odds fusion helpers that paper 3 and paper 4 use as the
//! scalar scoring substrate.

use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_scoring::{
    prob::confidence_scaled_log_odds_pool_weighted, BM25Params, BM25Scorer, BayesianBM25Params,
    BayesianBM25Scorer, RawBm25Score, VectorScorer,
};

fn stats() -> Arc<uqa_core::IndexStats> {
    let mut stats = uqa_core::IndexStats::default();
    stats.total_docs = 100_000;
    stats.avg_doc_length = 120.0;
    Arc::new(stats)
}

fn vector(seed: u64, dim: usize) -> Vec<f32> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    (0..dim)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            ((state >> 32) as u32 as f32) / (u32::MAX as f32)
        })
        .collect()
}

fn bench_bm25(c: &mut Criterion) {
    let bm25_scorer = BM25Scorer::new(BM25Params::default(), stats());
    c.bench_function("bm25_score_single", |bencher| {
        bencher.iter(|| black_box(bm25_scorer.score(5, 120, 1_000)));
    });
    c.bench_function("bm25_idf", |bencher| {
        bencher.iter(|| black_box(bm25_scorer.idf(500)));
    });
    let mut group = c.benchmark_group("bm25_combine_scores");
    for num_terms in [1_usize, 3, 5, 10] {
        let term_scores: Vec<f64> = (0..num_terms).map(|i| 0.1 + i as f64 * 0.05).collect();
        group.bench_with_input(
            BenchmarkId::from_parameter(num_terms),
            &term_scores,
            |bencher, term_scores| {
                bencher.iter(|| black_box(BM25Scorer::combine_scores(black_box(term_scores))));
            },
        );
    }
    group.finish();

    let corpus: Vec<(u64, u64, u64)> = (0..100_000)
        .map(|i| (1 + i % 9, 60 + i % 180, 200 + i % 8_000))
        .collect();
    c.bench_function("bm25_score_batch_100k", |bencher| {
        bencher.iter(|| {
            let total: f64 = corpus
                .iter()
                .map(|(tf, dl, df)| bm25_scorer.score(*tf, *dl, *df))
                .sum();
            black_box(total)
        });
    });
}

fn bench_bayesian_bm25(c: &mut Criterion) {
    let bayesian_scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), stats()).unwrap();
    c.bench_function("bayesian_bm25_score_single", |bencher| {
        bencher.iter(|| black_box(bayesian_scorer.score(5, 120, 1_000)));
    });
    let corpus: Vec<(u64, u64, u64)> = (0..100_000)
        .map(|i| (1 + i % 9, 60 + i % 180, 200 + i % 8_000))
        .collect();
    c.bench_function("bayesian_bm25_score_batch_100k", |bencher| {
        bencher.iter(|| {
            let total: f64 = corpus
                .iter()
                .map(|(tf, dl, df)| bayesian_scorer.score(*tf, *dl, *df))
                .sum();
            black_box(total)
        });
    });

    let mut group = c.benchmark_group("bayesian_bm25_combine_scores");
    for num_terms in [1_usize, 3, 5, 10] {
        let term_scores: Vec<RawBm25Score> = (0..num_terms)
            .map(|i| RawBm25Score::new(0.2 + i as f64 * 0.05).unwrap())
            .collect();
        group.bench_with_input(
            BenchmarkId::from_parameter(num_terms),
            &term_scores,
            |bencher, term_scores| {
                bencher.iter(|| black_box(bayesian_scorer.combine_scores(black_box(term_scores))));
            },
        );
    }
    group.finish();
}

fn bench_vector_scoring(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_cosine_similarity");
    for dim in [64_usize, 128, 256] {
        let a = vector(1, dim);
        let b = vector(2, dim);
        group.bench_with_input(BenchmarkId::from_parameter(dim), &dim, |bencher, _| {
            bencher.iter(|| {
                black_box(VectorScorer::cosine_similarity(
                    black_box(&a),
                    black_box(&b),
                ))
            });
        });
    }
    group.finish();

    c.bench_function("vector_similarity_to_probability", |bencher| {
        bencher.iter(|| black_box(VectorScorer::similarity_to_probability(black_box(0.85))));
    });

    let query = vector(0, 128);
    let corpus: Vec<Vec<f32>> = (0..10_000).map(|i| vector(i + 1, 128)).collect();
    c.bench_function("vector_cosine_batch_10k_dim128", |bencher| {
        bencher.iter(|| {
            let total: f64 = corpus
                .iter()
                .map(|v| VectorScorer::cosine_similarity(&query, v).unwrap())
                .sum();
            black_box(total)
        });
    });
}

fn bench_confidence_scaled_log_odds_pool(c: &mut Criterion) {
    let mut group = c.benchmark_group("confidence_scaled_log_odds_pool");
    for n_signals in [2_usize, 3, 5, 10] {
        let probs: Vec<f64> = (0..n_signals)
            .map(|i| 0.2 + (i as f64 + 1.0) / ((n_signals + 4) as f64))
            .collect();
        let weights = vec![1.0 / n_signals as f64; n_signals];
        group.bench_with_input(
            BenchmarkId::from_parameter(n_signals),
            &n_signals,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        confidence_scaled_log_odds_pool_weighted(
                            black_box(&probs),
                            black_box(&weights),
                            black_box(0.5),
                        )
                        .expect("weighted fusion"),
                    )
                });
            },
        );
    }
    group.finish();

    let samples: Vec<Vec<f64>> = (0..10_000)
        .map(|i| {
            vec![
                0.1 + f64::from(i % 80) / 100.0,
                0.2 + f64::from(i % 70) / 100.0,
                0.3 + f64::from(i % 60) / 100.0,
            ]
        })
        .collect();
    let weights = [0.4, 0.35, 0.25];
    c.bench_function("confidence_scaled_log_odds_pool_batch_10k", |bencher| {
        bencher.iter(|| {
            let total: f64 = samples
                .iter()
                .map(|probs| {
                    confidence_scaled_log_odds_pool_weighted(probs, &weights, 0.5)
                        .expect("batch benchmark probabilities and weights are valid")
                })
                .sum();
            black_box(total)
        });
    });
}

criterion_group!(
    benches,
    bench_bm25,
    bench_bayesian_bm25,
    bench_vector_scoring,
    bench_confidence_scaled_log_odds_pool
);
criterion_main!(benches);
