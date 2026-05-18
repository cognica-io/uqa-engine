//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! External-prior benchmarks mirroring UQA `bench_external_prior.py`.

use std::collections::BTreeMap;
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_core::Value;
use uqa_scoring::{
    authority_prior, recency_prior, BayesianBM25Params, ExternalPriorScorer, PriorFn,
};

fn stats() -> Arc<uqa_core::IndexStats> {
    let mut stats = uqa_core::IndexStats::default();
    stats.total_docs = 100_000;
    stats.avg_doc_length = 120.0;
    Arc::new(stats)
}

fn doc_fields(i: u64) -> BTreeMap<String, Value> {
    let mut fields = BTreeMap::new();
    let authority = match i % 3 {
        0 => "high",
        1 => "medium",
        _ => "low",
    };
    fields.insert("authority".to_string(), Value::Str(authority.to_string()));
    fields.insert(
        "timestamp".to_string(),
        Value::Str("2026-05-01T00:00:00Z".to_string()),
    );
    fields
}

fn scorer(prior: PriorFn) -> ExternalPriorScorer {
    ExternalPriorScorer::new(BayesianBM25Params::default(), stats(), prior)
}

fn bench_score_with_prior(c: &mut Criterion) {
    let scorer = scorer(authority_prior("authority", None));
    let fields = doc_fields(0);
    c.bench_function("external_prior_score_with_prior", |bencher| {
        bencher.iter(|| {
            black_box(scorer.score_with_prior(
                black_box(5),
                black_box(120),
                black_box(1_000),
                black_box(&fields),
            ))
        });
    });
}

fn bench_score_batch(c: &mut Criterion) {
    let scorer = scorer(authority_prior("authority", None));
    let mut group = c.benchmark_group("external_prior_score_batch");
    for n in [100_u64, 1_000] {
        let docs: Vec<_> = (0..n).map(doc_fields).collect();
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |bencher, _| {
            bencher.iter(|| {
                let total: f64 = docs
                    .iter()
                    .enumerate()
                    .map(|(i, fields)| {
                        scorer.score_with_prior(
                            1 + (i as u64 % 7),
                            50 + (i as u64 % 200),
                            100 + (i as u64 % 5_000),
                            fields,
                        )
                    })
                    .sum();
                black_box(total)
            });
        });
    }
    group.finish();
}

fn bench_prior_functions(c: &mut Criterion) {
    let recency = recency_prior("timestamp", 30.0);
    let authority = authority_prior("authority", None);
    let fields = doc_fields(0);
    c.bench_function("external_prior_recency_computation", |bencher| {
        bencher.iter(|| black_box(recency(black_box(&fields))));
    });
    c.bench_function("external_prior_authority_computation", |bencher| {
        bencher.iter(|| black_box(authority(black_box(&fields))));
    });
}

criterion_group!(
    benches,
    bench_score_with_prior,
    bench_score_batch,
    bench_prior_functions
);
criterion_main!(benches);
