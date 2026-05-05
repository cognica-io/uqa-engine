//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `AttentionFusion` and `MultiHeadAttentionFusion` property tests
//! (Phase 8, Paper 4 Section 8).
//!
//! Pins:
//! - `fuse` output in `[0, 1]` for any `(probs, query_features, weights)`,
//! - `n = 1` identity: `fuse([p], qf) == p`,
//! - empty input returns `0.5` (neutral fallback),
//! - zero weights produce uniform attention `1/n`, and at `alpha = 0`
//!   the fused output equals scale-neutral mean log-odds,
//! - multi-head with `k` identical heads equals a single head,
//! - multi-head averaged output sits between the min and max of the
//!   per-head outputs.

use proptest::prelude::*;
use uqa_fusion::{AttentionFusion, MultiHeadAttentionFusion};
use uqa_scoring::{logit, sigmoid};

fn safe_prob() -> impl Strategy<Value = f64> {
    1e-3f64..(1.0 - 1e-3)
}

/// `(probs, query_features)` of independently chosen lengths
/// (`n_signals` and `n_query_features`).
fn arb_inputs() -> impl Strategy<Value = (Vec<f64>, Vec<f64>)> {
    let probs = (1usize..=4).prop_flat_map(|n| proptest::collection::vec(safe_prob(), n..=n));
    let qf = (1usize..=4).prop_flat_map(|m| proptest::collection::vec(-1.0f64..1.0, m..=m));
    (probs, qf)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        ..ProptestConfig::default()
    })]

    /// `fuse` output in `[0, 1]` for any input. Uses zero-weights so
    /// the property holds across the full (probs, qf, alpha) space
    /// without needing to vary the weights matrix.
    #[test]
    fn fuse_output_in_unit_interval(
        (probs, qf) in arb_inputs(),
        alpha in 0.0f64..2.0,
    ) {
        let a = AttentionFusion::new(probs.len(), qf.len(), alpha);
        let out = a.fuse(&probs, &qf);
        prop_assert!((0.0..=1.0).contains(&out), "fuse returned {out}");
    }

    /// `n = 1` identity: a single signal feeds through unchanged.
    #[test]
    fn n1_identity(p in safe_prob(), qf_len in 1usize..=4, alpha in 0.0f64..2.0) {
        let a = AttentionFusion::new(1, qf_len, alpha);
        let qf = vec![0.5; qf_len];
        let got = a.fuse(&[p], &qf);
        prop_assert!((got - p).abs() < 1e-12, "fuse([{p}]) = {got}");
    }

    /// Zero raw weights at `alpha = 0` collapse to scale-neutral
    /// mean log-odds: `sigmoid(mean(logit p_i))`.
    #[test]
    fn zero_weights_at_alpha_zero_match_mean_log_odds(
        (probs, qf) in arb_inputs(),
    ) {
        let a = AttentionFusion::new(probs.len(), qf.len(), 0.0);
        let got = a.fuse(&probs, &qf);
        if probs.len() == 1 {
            prop_assert!((got - probs[0]).abs() < 1e-9);
        } else {
            let n = probs.len() as f64;
            let expected = sigmoid(probs.iter().map(|&p| logit(p)).sum::<f64>() / n);
            prop_assert!(
                (got - expected).abs() < 1e-9,
                "zero-weight fuse = {got} != mean log-odds = {expected}",
            );
        }
    }

    /// Multi-head with `k` identical heads (all freshly constructed
    /// with the same shape) equals a single head's output. Averaging
    /// the same value `k` times is the value.
    #[test]
    fn multi_head_with_identical_heads_matches_single(
        (probs, qf) in arb_inputs(),
        k in 1usize..=4,
        alpha in 0.0f64..2.0,
    ) {
        let single = AttentionFusion::new(probs.len(), qf.len(), alpha);
        let mh = MultiHeadAttentionFusion::new(k, probs.len(), qf.len(), alpha);
        let s = single.fuse(&probs, &qf);
        let m = mh.fuse(&probs, &qf);
        prop_assert!(
            (s - m).abs() < 1e-9,
            "multi-head ({k} heads) = {m} != single = {s}",
        );
    }

    /// Multi-head average sits between the min and max of the
    /// per-head outputs (basic mean property).
    #[test]
    fn multi_head_average_bounded_by_per_head(
        (probs, qf) in arb_inputs(),
        k in 1usize..=4,
        alpha in 0.0f64..2.0,
    ) {
        let mh = MultiHeadAttentionFusion::new(k, probs.len(), qf.len(), alpha);
        let per_head: Vec<f64> = mh.heads.iter().map(|h| h.fuse(&probs, &qf)).collect();
        let avg = mh.fuse(&probs, &qf);
        let lo = per_head.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = per_head.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        prop_assert!(
            (lo - 1e-12..=hi + 1e-12).contains(&avg),
            "avg {avg} not in [{lo}, {hi}]",
        );
    }
}

/// Empty inputs return the neutral fallback.
#[test]
fn empty_input_returns_neutral() {
    let a = AttentionFusion::new(0, 0, 0.5);
    assert_eq!(a.fuse(&[], &[]), 0.5);
}

/// `MultiHeadAttentionFusion::new(0, ...)` with zero heads yields the
/// neutral fallback (no heads to average over).
#[test]
fn multi_head_zero_heads_returns_neutral() {
    let mh = MultiHeadAttentionFusion::new(0, 2, 2, 0.0);
    assert_eq!(mh.fuse(&[0.7, 0.6], &[1.0, 0.0]), 0.5);
}

/// `attention_weights` from zero raw weights gives uniform `1/n`.
#[test]
fn zero_raw_weights_give_uniform_attention() {
    let a = AttentionFusion::new(4, 3, 0.0);
    let w = a.attention_weights(&[1.0, -0.5, 0.7]);
    for v in &w {
        assert!((v - 0.25).abs() < 1e-9, "expected 0.25, got {v}");
    }
}
