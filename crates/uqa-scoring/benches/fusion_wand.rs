//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fusion WAND benchmarks mirroring UQA `bench_scoring_advanced.py`.

use std::collections::BTreeMap;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_scoring::{fusion_wand::SignalScoreMap, FusionWANDScorer};

fn signal(n_docs: u64, signal_idx: u64) -> SignalScoreMap {
    let mut map = BTreeMap::new();
    for doc_id in 0..n_docs {
        if (doc_id + signal_idx) % (signal_idx + 2) == 0 {
            let score = 0.1 + ((doc_id * 17 + signal_idx * 31) % 850) as f64 / 1_000.0;
            map.insert(doc_id, score);
        }
    }
    map
}

fn signals(n_signals: usize, n_docs: u64) -> Vec<SignalScoreMap> {
    (0..n_signals).map(|i| signal(n_docs, i as u64)).collect()
}

fn bench_top_k(c: &mut Criterion) {
    let sigs = signals(3, 10_000);
    let bounds = vec![0.95; sigs.len()];
    let mut group = c.benchmark_group("fusion_wand_top_k");
    for k in [10_usize, 50, 100] {
        let scorer = FusionWANDScorer::new(sigs.clone(), bounds.clone(), 0.5, k).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(k), &k, |bencher, _| {
            bencher.iter(|| {
                let result = black_box(&scorer).score_top_k().unwrap();
                black_box(result.len())
            });
        });
    }
    group.finish();
}

fn bench_vs_exhaustive_shapes(c: &mut Criterion) {
    let mut group = c.benchmark_group("fusion_wand_by_signal_count");
    for n_signals in [2_usize, 3, 5] {
        let sigs = signals(n_signals, 10_000);
        let bounds = vec![0.95; sigs.len()];
        let scorer = FusionWANDScorer::new(sigs, bounds, 0.5, 50).unwrap();
        group.bench_with_input(
            BenchmarkId::from_parameter(n_signals),
            &n_signals,
            |bencher, _| {
                bencher.iter(|| {
                    let result = black_box(&scorer).score_top_k().unwrap();
                    black_box(result.len())
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_top_k, bench_vs_exhaustive_shapes);
criterion_main!(benches);
