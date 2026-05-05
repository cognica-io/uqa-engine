//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Microbenchmarks for the calibration metrics (Brier score and
//! log loss). Each bench runs over 100k synthetic
//! `(prediction, label)` pairs.
//!
//! Run with `cargo bench -p uqa-scoring --bench calibration`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_scoring::CalibrationMetrics;

const N: usize = 100_000;

fn build_dataset() -> (Vec<f64>, Vec<u8>) {
    let mut probs = Vec::with_capacity(N);
    let mut labels = Vec::with_capacity(N);
    // Deterministic synthetic: probabilities cycle through a sawtooth
    // over [0.05, 0.95]; labels alternate so log_loss / Brier touch
    // both extremes.
    for i in 0..N {
        let p = 0.05 + (i % 10) as f64 * 0.1;
        probs.push(p.clamp(0.05, 0.95));
        labels.push(u8::from((i % 3) != 0));
    }
    (probs, labels)
}

fn bench_log_loss(c: &mut Criterion) {
    let (probs, labels) = build_dataset();
    c.bench_function("calibration_log_loss_100k", |bencher| {
        bencher.iter(|| black_box(CalibrationMetrics::log_loss(&probs, &labels)));
    });
}

fn bench_brier(c: &mut Criterion) {
    let (probs, labels) = build_dataset();
    c.bench_function("calibration_brier_100k", |bencher| {
        bencher.iter(|| black_box(CalibrationMetrics::brier(&probs, &labels)));
    });
}

criterion_group!(benches, bench_log_loss, bench_brier);
criterion_main!(benches);
