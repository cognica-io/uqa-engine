//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Operator benchmarks mirroring UQA `bench_multi_field.py`,
//! `bench_scoring_advanced.py`, and the operator portions of paper 4
//! and paper 5 benchmark coverage.

use std::collections::BTreeMap;
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use uqa_analysis::standard_analyzer;
use uqa_core::{Payload, PostingEntry, PostingList};
use uqa_fusion::{AttentionFusion, LearnedFusion};
use uqa_operators::{
    AttentionFuser, AttentionFusionOperator, CalibratedVectorOperator, Cutoff, ExecutionContext,
    LearnedFusionOperator, MultiFieldSearchOperator, MultiStageOperator, Operator,
    ProgressiveFusionOperator, SparseThresholdOperator, WeightSource,
};
use uqa_storage::{InvertedIndex, MemoryInvertedIndex, MemoryVectorIndex, VectorIndex};

struct LiteralOperator(Vec<PostingEntry>);

impl Operator for LiteralOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> PostingList {
        PostingList::from_sorted_unchecked(self.0.clone())
    }
}

fn entry(doc_id: u64, score: f64) -> PostingEntry {
    PostingEntry::new(doc_id, Payload::with_score(score))
}

fn literal(size: u64, offset: u64) -> Arc<dyn Operator> {
    let entries = (0..size)
        .map(|i| entry(offset + i, 0.1 + ((i * 17) % 850) as f64 / 1_000.0))
        .collect();
    Arc::new(LiteralOperator(entries))
}

fn bench_sparse_threshold(c: &mut Criterion) {
    let mut size_group = c.benchmark_group("sparse_threshold_by_size");
    for size in [100_u64, 1_000, 10_000] {
        let op = SparseThresholdOperator::new(literal(size, 0), 0.3);
        let ctx = ExecutionContext::new();
        size_group.bench_with_input(BenchmarkId::from_parameter(size), &size, |bencher, _| {
            bencher.iter(|| {
                let result = black_box(&op).execute(black_box(&ctx));
                black_box(result.len())
            });
        });
    }
    size_group.finish();

    let mut threshold_group = c.benchmark_group("sparse_threshold_levels");
    for threshold in [0.1, 0.3, 0.5, 0.7] {
        let op = SparseThresholdOperator::new(literal(10_000, 0), threshold);
        let ctx = ExecutionContext::new();
        threshold_group.bench_with_input(
            BenchmarkId::from_parameter(format!("{threshold:.1}")),
            &threshold,
            |bencher, _| {
                bencher.iter(|| {
                    let result = black_box(&op).execute(black_box(&ctx));
                    black_box(result.len())
                });
            },
        );
    }
    threshold_group.finish();
}

fn bench_attention_and_learned_operators(c: &mut Criterion) {
    let ctx = ExecutionContext::new();
    let mut group = c.benchmark_group("attention_fusion_operator");
    for n_signals in [2_usize, 3, 5, 10] {
        let signals: Vec<Arc<dyn Operator>> = (0..n_signals)
            .map(|i| literal(1_000, (i * 100) as u64))
            .collect();
        let op = AttentionFusionOperator::new(
            signals,
            AttentionFuser::Single(AttentionFusion::new(n_signals, 6, 0.5)),
            vec![0.2, 0.4, 0.6, 0.8, 1.0, 0.5],
        );
        group.bench_with_input(
            BenchmarkId::from_parameter(n_signals),
            &n_signals,
            |bencher, _| {
                bencher.iter(|| {
                    let result = black_box(&op).execute(black_box(&ctx));
                    black_box(result.len())
                });
            },
        );
    }
    group.finish();

    let mut learned_group = c.benchmark_group("learned_fusion_operator");
    for n_signals in [2_usize, 3, 5, 10] {
        let signals: Vec<Arc<dyn Operator>> = (0..n_signals)
            .map(|i| literal(1_000, (i * 100) as u64))
            .collect();
        let op = LearnedFusionOperator::new(signals, LearnedFusion::new(n_signals, 0.5));
        learned_group.bench_with_input(
            BenchmarkId::from_parameter(n_signals),
            &n_signals,
            |bencher, _| {
                bencher.iter(|| {
                    let result = black_box(&op).execute(black_box(&ctx));
                    black_box(result.len())
                });
            },
        );
    }
    learned_group.finish();
}

fn bench_multi_stage(c: &mut Criterion) {
    let ctx = ExecutionContext::new();
    let mut size_group = c.benchmark_group("multi_stage_two_stage");
    for size in [100_u64, 1_000] {
        let op = MultiStageOperator::new(vec![
            (literal(size, 0), Cutoff::TopK((size / 2) as usize)),
            (literal(size, 0), Cutoff::TopK((size / 4) as usize)),
        ]);
        size_group.bench_with_input(BenchmarkId::from_parameter(size), &size, |bencher, _| {
            bencher.iter(|| {
                let result = black_box(&op).execute(black_box(&ctx));
                black_box(result.len())
            });
        });
    }
    size_group.finish();

    let op = MultiStageOperator::new(vec![
        (literal(1_000, 0), Cutoff::TopK(500)),
        (literal(1_000, 0), Cutoff::Threshold(0.4)),
        (literal(1_000, 0), Cutoff::TopK(100)),
    ]);
    c.bench_function("multi_stage_three_stage", |bencher| {
        bencher.iter(|| {
            let result = black_box(&op).execute(black_box(&ctx));
            black_box(result.len())
        });
    });
}

fn bench_progressive_fusion(c: &mut Criterion) {
    let ctx = ExecutionContext::new();
    let op = ProgressiveFusionOperator::new(
        vec![
            (vec![literal(1_000, 0), literal(1_000, 50)], 300),
            (vec![literal(800, 0)], 100),
        ],
        0.5,
    );
    c.bench_function("progressive_fusion_two_stage", |bencher| {
        bencher.iter(|| {
            let result = black_box(&op).execute(black_box(&ctx));
            black_box(result.len())
        });
    });
}

fn multi_field_context(n_docs: u64) -> ExecutionContext {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    for doc_id in 0..n_docs {
        let mut fields = BTreeMap::new();
        fields.insert("title".to_string(), format!("rust graph query {doc_id}"));
        fields.insert(
            "body".to_string(),
            format!("bayesian vector fusion graph query planner {doc_id}"),
        );
        idx.add_document(doc_id, fields);
    }
    ExecutionContext::new().with_inverted_index(idx.snapshot())
}

fn bench_multi_field_operator(c: &mut Criterion) {
    let ctx = multi_field_context(2_000);
    let mut group = c.benchmark_group("multi_field_operator_execute");
    for n_fields in [2_usize, 3] {
        let mut fields = vec!["title".to_string(), "body".to_string()];
        if n_fields == 3 {
            fields.push("missing".to_string());
        }
        let op = MultiFieldSearchOperator::new(fields, "graph query", None);
        group.bench_with_input(
            BenchmarkId::from_parameter(n_fields),
            &n_fields,
            |bencher, _| {
                bencher.iter(|| {
                    let result = black_box(&op).execute(black_box(&ctx));
                    black_box(result.len())
                });
            },
        );
    }
    group.finish();
}

fn vector_context(n_docs: u64) -> ExecutionContext {
    let mut idx = MemoryVectorIndex::new(16);
    for doc_id in 0..n_docs {
        let vector: Vec<f32> = (0..16)
            .map(|d| ((doc_id + d as u64 * 17) % 101) as f32 / 101.0)
            .collect();
        idx.add(doc_id, vector);
    }
    ExecutionContext::new().with_vector_index("embedding", idx.snapshot())
}

fn bench_calibrated_vector(c: &mut Criterion) {
    let ctx = vector_context(2_000);
    let query: Vec<f32> = (0..16).map(|d| d as f32 / 16.0).collect();
    let uniform = CalibratedVectorOperator::new(query.clone(), 100, "embedding");
    let gap = CalibratedVectorOperator::new(query, 100, "embedding")
        .with_weight_source(WeightSource::DistanceGap);
    c.bench_function("calibrated_vector_uniform", |bencher| {
        bencher.iter(|| {
            let result = black_box(&uniform).execute(black_box(&ctx));
            black_box(result.len())
        });
    });
    c.bench_function("calibrated_vector_distance_gap", |bencher| {
        bencher.iter(|| {
            let result = black_box(&gap).execute(black_box(&ctx));
            black_box(result.len())
        });
    });
}

criterion_group!(
    benches,
    bench_sparse_threshold,
    bench_attention_and_learned_operators,
    bench_multi_stage,
    bench_progressive_fusion,
    bench_multi_field_operator,
    bench_calibrated_vector
);
criterion_main!(benches);
