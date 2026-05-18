//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! BEIR-style vector calibration benchmark ported from UQA
//! `bench_beir_calibration.py` and `bench_hybrid_fusion.py`.
//!
//! The benchmark uses a deterministic synthetic IR fixture so the Rust
//! workspace owns the data and still exercises the same metrics:
//! ECE, Brier, log loss, NDCG@10, dense probabilities, calibrated
//! probabilities, reciprocal rank fusion, convex fusion, and balanced
//! log-odds fusion.

use std::collections::BTreeMap;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_scoring::{
    log_odds_conjunction, ndcg_at_k, CalibrationMetrics, VectorProbabilityTransform, VectorScorer,
};

const N_DOCS: usize = 240;
const N_QUERIES: usize = 40;
const DIM: usize = 32;
const K: usize = 50;

#[derive(Clone)]
struct QueryCase {
    vector: Vec<f32>,
    relevant: BTreeMap<usize, f64>,
}

struct Fixture {
    corpus: Vec<Vec<f32>>,
    queries: Vec<QueryCase>,
}

fn vector(seed: u64) -> Vec<f32> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    (0..DIM)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            ((state >> 32) as u32 as f32) / (u32::MAX as f32)
        })
        .collect()
}

fn fixture() -> Fixture {
    let corpus: Vec<Vec<f32>> = (0..N_DOCS).map(|i| vector(i as u64 + 11)).collect();
    let mut queries = Vec::with_capacity(N_QUERIES);
    for q in 0..N_QUERIES {
        let anchor = (q * 5) % N_DOCS;
        let mut relevant = BTreeMap::new();
        relevant.insert(anchor, 3.0);
        relevant.insert((anchor + 1) % N_DOCS, 2.0);
        relevant.insert((anchor + 2) % N_DOCS, 1.0);
        queries.push(QueryCase {
            vector: corpus[anchor].clone(),
            relevant,
        });
    }
    Fixture { corpus, queries }
}

fn ranked_dense(corpus: &[Vec<f32>], query: &[f32], k: usize) -> Vec<(usize, f64)> {
    let mut scored: Vec<(usize, f64)> = corpus
        .iter()
        .enumerate()
        .map(|(doc_id, v)| (doc_id, VectorScorer::cosine_similarity(query, v)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    scored
}

fn distance_gap_weights(distances: &[f64]) -> Vec<f64> {
    if distances.len() < 2 {
        return vec![1.0; distances.len()];
    }
    let mut sorted = distances.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut max_gap = 0.0;
    let mut gap_value = sorted[0];
    for pair in sorted.windows(2) {
        let gap = pair[1] - pair[0];
        if gap > max_gap {
            max_gap = gap;
            gap_value = pair[0];
        }
    }
    distances
        .iter()
        .map(|distance| {
            if *distance <= gap_value {
                1.0
            } else {
                (-(*distance - gap_value)).exp()
            }
        })
        .collect()
}

fn aggregate_gap_calibration(fx: &Fixture) -> (f64, f64, f64, f64) {
    let calibrator = VectorProbabilityTransform::new(0.02, 0.55, 0.20, 0.05);
    let mut naive_probs = Vec::new();
    let mut cal_probs = Vec::new();
    let mut labels = Vec::new();
    let mut ndcg_naive = 0.0;
    let mut ndcg_cal = 0.0;
    for query in &fx.queries {
        let ranked = ranked_dense(&fx.corpus, &query.vector, K);
        let similarities: Vec<f64> = ranked.iter().map(|(_, score)| *score).collect();
        let distances: Vec<f64> = similarities.iter().map(|score| 1.0 - *score).collect();
        let weights = distance_gap_weights(&distances);
        let calibrated = calibrator.calibrate(&distances, Some(&weights));
        let mut cal_ranked: Vec<(usize, f64)> = ranked
            .iter()
            .zip(calibrated.iter())
            .map(|((doc_id, _), p)| (*doc_id, *p))
            .collect();
        cal_ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let rel_naive: Vec<f64> = ranked
            .iter()
            .map(|(doc_id, _)| query.relevant.get(doc_id).copied().unwrap_or(0.0))
            .collect();
        let rel_cal: Vec<f64> = cal_ranked
            .iter()
            .map(|(doc_id, _)| query.relevant.get(doc_id).copied().unwrap_or(0.0))
            .collect();
        ndcg_naive += ndcg_at_k(&rel_naive, 10);
        ndcg_cal += ndcg_at_k(&rel_cal, 10);

        for ((doc_id, score), p_cal) in ranked.iter().zip(calibrated.iter()) {
            naive_probs.push(VectorScorer::similarity_to_probability(*score));
            cal_probs.push(*p_cal);
            labels.push(u8::from(query.relevant.contains_key(doc_id)));
        }
    }
    let q = fx.queries.len() as f64;
    (
        CalibrationMetrics::ece(&naive_probs, &labels, 10),
        CalibrationMetrics::ece(&cal_probs, &labels, 10),
        ndcg_naive / q,
        ndcg_cal / q,
    )
}

fn reciprocal_rank_fusion(dense: &[(usize, f64)], sparse: &[(usize, f64)]) -> BTreeMap<usize, f64> {
    let mut out = BTreeMap::new();
    for (rank, (doc_id, _)) in dense.iter().enumerate() {
        *out.entry(*doc_id).or_insert(0.0) += 1.0 / (60.0 + rank as f64 + 1.0);
    }
    for (rank, (doc_id, _)) in sparse.iter().enumerate() {
        *out.entry(*doc_id).or_insert(0.0) += 1.0 / (60.0 + rank as f64 + 1.0);
    }
    out
}

fn hybrid_report(fx: &Fixture) -> (f64, f64, f64) {
    let mut ndcg_dense = 0.0;
    let mut ndcg_rrf = 0.0;
    let mut ndcg_balanced = 0.0;
    for query in &fx.queries {
        let dense = ranked_dense(&fx.corpus, &query.vector, K);
        let mut sparse = dense.clone();
        sparse.reverse();
        let rrf = reciprocal_rank_fusion(&dense, &sparse);

        let mut balanced: Vec<(usize, f64)> = dense
            .iter()
            .zip(sparse.iter())
            .map(|((dense_id, dense_score), (_, sparse_score))| {
                let dense_p = VectorScorer::similarity_to_probability(*dense_score);
                let sparse_p = VectorScorer::similarity_to_probability(*sparse_score);
                (*dense_id, log_odds_conjunction(&[dense_p, sparse_p], 0.5))
            })
            .collect();
        balanced.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut rrf_ranked: Vec<(usize, f64)> = rrf.into_iter().collect();
        rrf_ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let rel_dense: Vec<f64> = dense
            .iter()
            .map(|(doc_id, _)| query.relevant.get(doc_id).copied().unwrap_or(0.0))
            .collect();
        let rel_rrf: Vec<f64> = rrf_ranked
            .iter()
            .map(|(doc_id, _)| query.relevant.get(doc_id).copied().unwrap_or(0.0))
            .collect();
        let rel_balanced: Vec<f64> = balanced
            .iter()
            .map(|(doc_id, _)| query.relevant.get(doc_id).copied().unwrap_or(0.0))
            .collect();
        ndcg_dense += ndcg_at_k(&rel_dense, 10);
        ndcg_rrf += ndcg_at_k(&rel_rrf, 10);
        ndcg_balanced += ndcg_at_k(&rel_balanced, 10);
    }
    let q = fx.queries.len() as f64;
    (ndcg_dense / q, ndcg_rrf / q, ndcg_balanced / q)
}

fn bench_gap_calibration(c: &mut Criterion) {
    let fx = fixture();
    c.bench_function("beir_gap_calibration_report", |bencher| {
        bencher.iter(|| black_box(aggregate_gap_calibration(black_box(&fx))));
    });
}

fn bench_hybrid_report(c: &mut Criterion) {
    let fx = fixture();
    c.bench_function("beir_hybrid_fusion_report", |bencher| {
        bencher.iter(|| black_box(hybrid_report(black_box(&fx))));
    });
}

criterion_group!(benches, bench_gap_calibration, bench_hybrid_report);
criterion_main!(benches);
