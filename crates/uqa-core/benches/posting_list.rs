//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Microbenchmarks for the `PostingList` two-pointer Boolean algebra.
//!
//! Run with `cargo bench -p uqa-core --bench posting_list`.
//! This mirrors the canonical UQA `bench_posting_list.py` surface:
//! pairwise Boolean operations by input size and overlap, top-k,
//! n-way merge, scored-payload union, and binary-search lookup.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_core::{Payload, PostingEntry, PostingList};

const N: u64 = 100_000;

fn posting_list(size: u64, start: u64, stride: u64, scored: bool) -> PostingList {
    let entries: Vec<PostingEntry> = (0..size)
        .map(|i| {
            let score = if scored {
                ((i % 1_000) as f64) / 1_000.0
            } else {
                0.5
            };
            PostingEntry::new(start + i * stride, Payload::with_score(score))
        })
        .collect();
    PostingList::from_sorted_unchecked(entries)
}

fn build_pair(size: u64, overlap: f64) -> (PostingList, PostingList) {
    let shared = ((size as f64) * overlap).round() as u64;
    let mut a = Vec::with_capacity(size as usize);
    let mut b = Vec::with_capacity(size as usize);
    for i in 0..size {
        a.push(PostingEntry::new(i, Payload::with_score(0.5)));
    }
    for i in 0..shared {
        b.push(PostingEntry::new(i, Payload::with_score(0.6)));
    }
    for i in shared..size {
        b.push(PostingEntry::new(
            size + (i - shared),
            Payload::with_score(0.6),
        ));
    }
    b.sort_by_key(|e| e.doc_id);
    (
        PostingList::from_sorted_unchecked(a),
        PostingList::from_sorted_unchecked(b),
    )
}

fn bench_union(c: &mut Criterion) {
    let mut group = c.benchmark_group("posting_list_union_by_size");
    for size in [1_000, 10_000, 100_000] {
        let (a, b) = build_pair(size, 0.3);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |bencher, _| {
            bencher.iter(|| {
                let result = black_box(&a).union(black_box(&b));
                black_box(result.len())
            });
        });
    }
    group.finish();

    let mut overlap_group = c.benchmark_group("posting_list_union_by_overlap");
    for overlap in [0.0, 0.3, 0.7, 1.0] {
        let (a, b) = build_pair(10_000, overlap);
        overlap_group.bench_with_input(
            BenchmarkId::from_parameter(format!("{overlap:.1}")),
            &overlap,
            |bencher, _| {
                bencher.iter(|| {
                    let result = black_box(&a).union(black_box(&b));
                    black_box(result.len())
                });
            },
        );
    }
    overlap_group.finish();
}

fn bench_intersect(c: &mut Criterion) {
    let mut group = c.benchmark_group("posting_list_intersect_by_size");
    for size in [1_000, 10_000, 100_000] {
        let (a, b) = build_pair(size, 0.3);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |bencher, _| {
            bencher.iter(|| {
                let result = black_box(&a).intersect(black_box(&b));
                black_box(result.len())
            });
        });
    }
    group.finish();

    let mut overlap_group = c.benchmark_group("posting_list_intersect_by_overlap");
    for overlap in [0.0, 0.3, 0.7, 1.0] {
        let (a, b) = build_pair(10_000, overlap);
        overlap_group.bench_with_input(
            BenchmarkId::from_parameter(format!("{overlap:.1}")),
            &overlap,
            |bencher, _| {
                bencher.iter(|| {
                    let result = black_box(&a).intersect(black_box(&b));
                    black_box(result.len())
                });
            },
        );
    }
    overlap_group.finish();
}

fn bench_difference(c: &mut Criterion) {
    let mut group = c.benchmark_group("posting_list_difference_by_size");
    for size in [1_000, 10_000, 100_000] {
        let (a, b) = build_pair(size, 0.3);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |bencher, _| {
            bencher.iter(|| {
                let result = black_box(&a).difference(black_box(&b));
                black_box(result.len())
            });
        });
    }
    group.finish();

    let mut overlap_group = c.benchmark_group("posting_list_difference_by_overlap");
    for overlap in [0.0, 0.3, 0.7, 1.0] {
        let (a, b) = build_pair(10_000, overlap);
        overlap_group.bench_with_input(
            BenchmarkId::from_parameter(format!("{overlap:.1}")),
            &overlap,
            |bencher, _| {
                bencher.iter(|| {
                    let result = black_box(&a).difference(black_box(&b));
                    black_box(result.len())
                });
            },
        );
    }
    overlap_group.finish();
}

fn bench_top_k(c: &mut Criterion) {
    let pl = posting_list(N, 0, 1, true);
    let mut group = c.benchmark_group("posting_list_top_k");
    for k in [10, 100, 1_000] {
        group.bench_with_input(BenchmarkId::from_parameter(k), &k, |bencher, k| {
            bencher.iter(|| {
                let result = black_box(&pl).top_k(*k);
                black_box(result.len())
            });
        });
    }
    group.finish();
}

fn bench_multi_merge(c: &mut Criterion) {
    let mut union_group = c.benchmark_group("posting_list_nway_union");
    for n_lists in [2_u64, 4, 8, 16] {
        let lists: Vec<PostingList> = (0..n_lists)
            .map(|i| posting_list(10_000, i * 2_500, 1, true))
            .collect();
        union_group.bench_with_input(
            BenchmarkId::from_parameter(n_lists),
            &n_lists,
            |bencher, _| {
                bencher.iter(|| {
                    let result = lists
                        .iter()
                        .skip(1)
                        .fold(lists[0].clone(), |acc, pl| acc.union(pl));
                    black_box(result.len())
                });
            },
        );
    }
    union_group.finish();

    let mut intersect_group = c.benchmark_group("posting_list_nway_intersect");
    for n_lists in [2_u64, 4, 8, 16] {
        let lists: Vec<PostingList> = (0..n_lists)
            .map(|i| posting_list(10_000, i * 1_000, 1, true))
            .collect();
        intersect_group.bench_with_input(
            BenchmarkId::from_parameter(n_lists),
            &n_lists,
            |bencher, _| {
                bencher.iter(|| {
                    let result = lists
                        .iter()
                        .skip(1)
                        .fold(lists[0].clone(), |acc, pl| acc.intersect(pl));
                    black_box(result.len())
                });
            },
        );
    }
    intersect_group.finish();
}

fn bench_payload_merge(c: &mut Criterion) {
    let (a, b) = build_pair(10_000, 0.5);
    c.bench_function("posting_list_union_with_scores_10k", |bencher| {
        bencher.iter(|| {
            let result = black_box(&a).union(black_box(&b));
            black_box(result.len())
        });
    });

    let pl = posting_list(N, 0, 1, true);
    let target = pl.entries()[50_000].doc_id;
    c.bench_function("posting_list_get_entry_binary_search_100k", |bencher| {
        bencher.iter(|| {
            let result = black_box(&pl).get_entry(black_box(target));
            black_box(result.map(|e| e.doc_id))
        });
    });
}

criterion_group!(
    benches,
    bench_union,
    bench_intersect,
    bench_difference,
    bench_top_k,
    bench_multi_merge,
    bench_payload_merge
);
criterion_main!(benches);
