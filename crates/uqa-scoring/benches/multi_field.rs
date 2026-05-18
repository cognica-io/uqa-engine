//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-field Bayesian scoring benchmarks mirroring UQA
//! `bench_multi_field.py`.

use std::collections::BTreeMap;
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_scoring::{BayesianBM25Params, FieldConfig, MultiFieldBayesianScorer};

fn stats() -> Arc<uqa_core::IndexStats> {
    let mut stats = uqa_core::IndexStats::default();
    stats.total_docs = 100_000;
    stats.avg_doc_length = 120.0;
    Arc::new(stats)
}

fn scorer(n_fields: usize) -> MultiFieldBayesianScorer {
    let configs = (0..n_fields)
        .map(|i| FieldConfig {
            field: format!("field_{i}"),
            params: BayesianBM25Params::default(),
            weight: 1.0 + i as f64 * 0.1,
        })
        .collect();
    MultiFieldBayesianScorer::new(configs, &stats())
}

fn field_maps(
    n_fields: usize,
    doc: u64,
) -> (
    BTreeMap<String, u64>,
    BTreeMap<String, u64>,
    BTreeMap<String, u64>,
) {
    let mut tf = BTreeMap::new();
    let mut dl = BTreeMap::new();
    let mut df = BTreeMap::new();
    for i in 0..n_fields {
        let key = format!("field_{i}");
        tf.insert(key.clone(), 1 + (doc + i as u64) % 7);
        dl.insert(key.clone(), 40 + (doc + 3 * i as u64) % 240);
        df.insert(key, 100 + (doc + 11 * i as u64) % 10_000);
    }
    (tf, dl, df)
}

fn bench_score_document(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_field_score_document");
    for n_fields in [2_usize, 3, 5] {
        let scorer = scorer(n_fields);
        let (tf, dl, df) = field_maps(n_fields, 1);
        group.bench_with_input(
            BenchmarkId::from_parameter(n_fields),
            &n_fields,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(scorer.score_document(black_box(&tf), black_box(&dl), black_box(&df)))
                });
            },
        );
    }
    group.finish();
}

fn bench_score_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_field_score_batch_1000");
    for n_fields in [2_usize, 5] {
        let scorer = scorer(n_fields);
        let docs: Vec<_> = (0..1_000).map(|doc| field_maps(n_fields, doc)).collect();
        group.bench_with_input(
            BenchmarkId::from_parameter(n_fields),
            &n_fields,
            |bencher, _| {
                bencher.iter(|| {
                    let total: f64 = docs
                        .iter()
                        .map(|(tf, dl, df)| scorer.score_document(tf, dl, df))
                        .sum();
                    black_box(total)
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_score_document, bench_score_batch);
criterion_main!(benches);
