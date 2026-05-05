//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Microbenchmarks for the `PostingList` two-pointer Boolean algebra.
//!
//! Run with `cargo bench -p uqa-core --bench posting_list`. The two
//! benches build a deterministic 100k-doc-id posting list pair and
//! measure `union` and `intersect` throughput.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_core::{Payload, PostingEntry, PostingList};

const N: u64 = 100_000;

fn build_pair() -> (PostingList, PostingList) {
    // List A: every even doc id in [0, 2*N).
    let entries_a: Vec<PostingEntry> = (0..N)
        .map(|i| PostingEntry::new(i * 2, Payload::with_score(0.5)))
        .collect();
    // List B: every third doc id in [0, 3*N).
    let entries_b: Vec<PostingEntry> = (0..N)
        .map(|i| PostingEntry::new(i * 3, Payload::with_score(0.5)))
        .collect();
    (
        PostingList::from_sorted_unchecked(entries_a),
        PostingList::from_sorted_unchecked(entries_b),
    )
}

fn bench_union(c: &mut Criterion) {
    let (a, b) = build_pair();
    c.bench_function("posting_list_union_100k", |bencher| {
        bencher.iter(|| {
            let result = black_box(&a).union(black_box(&b));
            black_box(result.len())
        });
    });
}

fn bench_intersect(c: &mut Criterion) {
    let (a, b) = build_pair();
    c.bench_function("posting_list_intersect_100k", |bencher| {
        bencher.iter(|| {
            let result = black_box(&a).intersect(black_box(&b));
            black_box(result.len())
        });
    });
}

criterion_group!(benches, bench_union, bench_intersect);
criterion_main!(benches);
