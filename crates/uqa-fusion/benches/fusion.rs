//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bayesian, robust-positive, attention, and learned fusion benchmarks.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_fusion::{
    AttentionFusion, BayesianEvidenceFusion, LearnedFusion, QueryFeatureExtractor,
    RobustPositiveEvidencePool,
};
use uqa_scoring::EvidenceLogit;

fn probabilities(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.15 + (i as f64 + 1.0) / (n as f64 + 4.0) * 0.7)
        .collect()
}

fn query_features(n: usize) -> Vec<f64> {
    (0..n).map(|i| (i as f64 + 1.0) / n as f64).collect()
}

fn bench_positive_evidence_pool(c: &mut Criterion) {
    let mut group = c.benchmark_group("robust_positive_evidence_pool");
    for n_signals in [2_usize, 3, 5, 10] {
        let probs = probabilities(n_signals);
        let weights = vec![1.0 / n_signals as f64; n_signals];
        let fusion = RobustPositiveEvidencePool::new(0.5).expect("benchmark alpha is valid");
        group.bench_with_input(
            BenchmarkId::from_parameter(n_signals),
            &n_signals,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        fusion
                            .fuse_weighted(black_box(&probs), black_box(&weights))
                            .expect("weighted fusion"),
                    )
                });
            },
        );
    }
    group.finish();

    let fusion = RobustPositiveEvidencePool::new(0.5).expect("benchmark alpha is valid");
    let samples: Vec<Vec<f64>> = (0..10_000).map(|i| probabilities(2 + i % 4)).collect();
    c.bench_function("robust_positive_evidence_pool_batch_10k", |bencher| {
        bencher.iter(|| {
            let total: f64 = samples.iter().map(|p| fusion.fuse(p)).sum();
            black_box(total)
        });
    });
}

fn bench_bayesian_evidence(c: &mut Criterion) {
    let mut group = c.benchmark_group("bayesian_evidence_fusion");
    let fusion = BayesianEvidenceFusion::new(0.05).expect("benchmark prior is valid");
    for n_signals in [2_usize, 3, 5, 10] {
        let evidence: Vec<_> = (0..n_signals)
            .map(|index| EvidenceLogit::new(index as f64 * 0.2 - 0.4).unwrap())
            .collect();
        group.bench_with_input(
            BenchmarkId::from_parameter(n_signals),
            &n_signals,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        fusion
                            .fuse(black_box(&evidence))
                            .expect("finite evidence sum"),
                    )
                });
            },
        );
    }
    group.finish();
}

fn bench_attention_fusion(c: &mut Criterion) {
    let mut group = c.benchmark_group("fusion_attention");
    for n_signals in [2_usize, 3, 5, 10] {
        let fusion = AttentionFusion::new(n_signals, 6, 0.5);
        let probs = probabilities(n_signals);
        let features = query_features(6);
        group.bench_with_input(
            BenchmarkId::from_parameter(n_signals),
            &n_signals,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        fusion
                            .fuse(black_box(&probs), black_box(&features))
                            .expect("benchmark shapes match"),
                    )
                });
            },
        );
    }
    group.finish();

    let mut stats = uqa_core::IndexStats::default();
    stats.total_docs = 10_000;
    stats.set_doc_freq("body", "graph", 500);
    stats.set_doc_freq("body", "bayesian", 300);
    stats.set_doc_freq("body", "vector", 800);
    let extractor = QueryFeatureExtractor::new(stats).with_field("body");
    let terms = vec![
        "graph".to_string(),
        "bayesian".to_string(),
        "vector".to_string(),
    ];
    c.bench_function("fusion_query_feature_extract", |bencher| {
        bencher.iter(|| black_box(extractor.extract(black_box(&terms))));
    });
}

fn bench_learned_fusion(c: &mut Criterion) {
    let mut group = c.benchmark_group("fusion_learned");
    for n_signals in [2_usize, 3, 5, 10] {
        let fusion = LearnedFusion::new(n_signals, 0.5);
        let probs = probabilities(n_signals);
        group.bench_with_input(
            BenchmarkId::from_parameter(n_signals),
            &n_signals,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        fusion
                            .fuse(black_box(&probs))
                            .expect("benchmark shapes match"),
                    )
                });
            },
        );
    }
    group.finish();

    let probs: Vec<Vec<f64>> = (0..1_000)
        .map(|i| {
            if i % 2 == 0 {
                vec![0.9, 0.5]
            } else {
                vec![0.1, 0.5]
            }
        })
        .collect();
    let labels: Vec<f64> = (0..1_000)
        .map(|i| if i % 2 == 0 { 1.0 } else { 0.0 })
        .collect();
    c.bench_function("fusion_learned_fit_1000", |bencher| {
        bencher.iter(|| {
            let mut fusion = LearnedFusion::new(2, 0.0);
            fusion
                .fit(
                    black_box(&probs),
                    black_box(&labels),
                    black_box(0.1),
                    black_box(10),
                )
                .expect("benchmark shapes match");
            black_box(fusion.weights[0])
        });
    });
}

criterion_group!(
    benches,
    bench_positive_evidence_pool,
    bench_bayesian_evidence,
    bench_attention_fusion,
    bench_learned_fusion
);
criterion_main!(benches);
