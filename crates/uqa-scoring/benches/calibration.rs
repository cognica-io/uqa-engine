//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Calibration benchmarks mirroring UQA `bench_calibration.py`.
//! Each metric bench runs over synthetic `(prediction, label)` pairs
//! and the parameter-learner benches cover batch fit plus streaming
//! updates.
//!
//! Run with `cargo bench -p uqa-scoring --bench calibration`.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_scoring::{CalibrationMetrics, ParameterLearner};

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

fn bench_ece(c: &mut Criterion) {
    let mut group = c.benchmark_group("calibration_ece");
    for n in [100_usize, 1_000, 10_000] {
        let (probs, labels) = build_dataset();
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |bencher, n| {
            bencher.iter(|| {
                black_box(CalibrationMetrics::ece(
                    black_box(&probs[..*n]),
                    black_box(&labels[..*n]),
                    black_box(10),
                ))
            });
        });
    }
    group.finish();
}

fn bench_reliability_diagram(c: &mut Criterion) {
    let mut group = c.benchmark_group("calibration_reliability_diagram");
    for n in [100_usize, 1_000] {
        let (probs, labels) = build_dataset();
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |bencher, n| {
            bencher.iter(|| {
                let result = CalibrationMetrics::reliability_diagram(
                    black_box(&probs[..*n]),
                    black_box(&labels[..*n]),
                    black_box(10),
                );
                black_box(result.len())
            });
        });
    }
    group.finish();
}

fn bench_report(c: &mut Criterion) {
    let mut group = c.benchmark_group("calibration_report");
    for n in [100_usize, 1_000] {
        let (probs, labels) = build_dataset();
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |bencher, n| {
            bencher.iter(|| {
                let report = CalibrationMetrics::report(
                    black_box(&probs[..*n]),
                    black_box(&labels[..*n]),
                    black_box(10),
                );
                black_box(report.ece + report.brier + report.log_loss)
            });
        });
    }
    group.finish();
}

fn learner_dataset(n: usize) -> (Vec<f64>, Vec<f64>) {
    let scores: Vec<f64> = (0..n)
        .map(|i| if i % 2 == 0 { 4.0 } else { -4.0 })
        .collect();
    let labels: Vec<f64> = (0..n).map(|i| if i % 2 == 0 { 1.0 } else { 0.0 }).collect();
    (scores, labels)
}

fn bench_parameter_learner(c: &mut Criterion) {
    let mut fit_group = c.benchmark_group("parameter_learner_fit");
    for n in [100_usize, 1_000] {
        let (scores, labels) = learner_dataset(n);
        fit_group.bench_with_input(BenchmarkId::from_parameter(n), &n, |bencher, _| {
            bencher.iter(|| {
                let mut learner = ParameterLearner::new(0.5, 0.0, Some(0.5));
                let params = learner.fit(
                    black_box(&scores),
                    black_box(&labels),
                    black_box(0.1),
                    black_box(10),
                );
                black_box(params.len())
            });
        });
    }
    fit_group.finish();

    c.bench_function("parameter_learner_update_single", |bencher| {
        bencher.iter(|| {
            let mut learner = ParameterLearner::new(0.5, 0.0, Some(0.5));
            learner.update(black_box(5.0), black_box(1.0), black_box(0.01));
            black_box(learner.alpha())
        });
    });

    let (scores, labels) = learner_dataset(1_000);
    c.bench_function("parameter_learner_update_stream_1000", |bencher| {
        bencher.iter(|| {
            let mut learner = ParameterLearner::new(0.5, 0.0, Some(0.5));
            for (&score, &label) in scores.iter().zip(labels.iter()) {
                learner.update(score, label, 0.01);
            }
            black_box(learner.alpha())
        });
    });
}

criterion_group!(
    benches,
    bench_log_loss,
    bench_brier,
    bench_ece,
    bench_reliability_diagram,
    bench_report,
    bench_parameter_learner
);
criterion_main!(benches);
