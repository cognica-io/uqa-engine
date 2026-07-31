//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Microbenchmarks for the semantic carrier layers introduced above posting
//! storage: document sets and finite-support semiring relations.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_core::{DocSet, LogSemiring, Relation, RelationEntry};

fn support_pair(size: u64, overlap: f64) -> (DocSet, DocSet) {
    let shared = ((size as f64) * overlap).round() as u64;
    let left = DocSet::from((0..size).collect::<Vec<_>>());
    let right = DocSet::from(
        (0..shared)
            .chain(size..size + (size - shared))
            .collect::<Vec<_>>(),
    );
    (left, right)
}

fn log_relation(support: &DocSet, weight: f64) -> Relation<LogSemiring> {
    let value = LogSemiring::from_weight(weight).expect("benchmark weight is non-negative");
    Relation::from_terms(
        support
            .iter()
            .map(|doc_id| RelationEntry::new(doc_id, value)),
    )
}

fn bench_doc_set(c: &mut Criterion) {
    let mut group = c.benchmark_group("doc_set_by_size");
    for size in [1_000, 10_000, 100_000] {
        let (left, right) = support_pair(size, 0.3);

        group.bench_with_input(BenchmarkId::new("union", size), &size, |bencher, _| {
            bencher.iter(|| {
                let result = black_box(&left).union(black_box(&right));
                black_box(result.len())
            });
        });
        group.bench_with_input(
            BenchmarkId::new("intersection", size),
            &size,
            |bencher, _| {
                bencher.iter(|| {
                    let result = black_box(&left).intersect(black_box(&right));
                    black_box(result.len())
                });
            },
        );
        group.bench_with_input(BenchmarkId::new("difference", size), &size, |bencher, _| {
            bencher.iter(|| {
                let result = black_box(&left).difference(black_box(&right));
                black_box(result.len())
            });
        });
    }
    group.finish();
}

fn bench_boolean_relation(c: &mut Criterion) {
    let (left_support, right_support) = support_pair(100_000, 0.3);
    let left = Relation::<bool>::from_support(&left_support);
    let right = Relation::<bool>::from_support(&right_support);

    let mut group = c.benchmark_group("boolean_relation_100k");
    group.bench_function("plus", |bencher| {
        bencher.iter(|| {
            let result = black_box(&left).plus(black_box(&right));
            black_box(result.len())
        });
    });
    group.bench_function("times", |bencher| {
        bencher.iter(|| {
            let result = black_box(&left).times(black_box(&right));
            black_box(result.len())
        });
    });
    group.finish();
}

fn bench_log_relation(c: &mut Criterion) {
    let (left_support, right_support) = support_pair(100_000, 0.3);
    let left = log_relation(&left_support, 0.4);
    let right = log_relation(&right_support, 0.6);

    let mut group = c.benchmark_group("log_relation_100k");
    group.bench_function("plus", |bencher| {
        bencher.iter(|| {
            let result = black_box(&left).plus(black_box(&right));
            black_box(result.len())
        });
    });
    group.bench_function("times", |bencher| {
        bencher.iter(|| {
            let result = black_box(&left).times(black_box(&right));
            black_box(result.len())
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_doc_set,
    bench_boolean_relation,
    bench_log_relation
);
criterion_main!(benches);
