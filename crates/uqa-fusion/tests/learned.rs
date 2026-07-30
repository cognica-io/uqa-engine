//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `LearnedFusion` property tests (Paper 4 Section 8).
//!
//! Pins:
//! - `n = 0` and model/input arity mismatches return an error,
//! - `n = 1` returns the single probability unchanged,
//! - `fuse` output is in `[0, 1]` for any input,
//! - when raw weights are all equal, `fuse` collapses to the
//!   uniform-weight log-odds (and at `alpha = 0` this is the
//!   scale-neutral mean log-odds: `sigmoid(mean(logit p_i))`),
//! - `fuse` is invariant under simultaneous permutation of
//!   probabilities and raw weights.

use proptest::prelude::*;
use uqa_fusion::LearnedFusion;
use uqa_scoring::{logit, sigmoid};

fn safe_prob() -> impl Strategy<Value = f64> {
    1e-3f64..(1.0 - 1e-3)
}

/// Tied-length `(probs, weights)` pair.
fn arb_probs_weights() -> impl Strategy<Value = (Vec<f64>, Vec<f64>)> {
    (1usize..=5).prop_flat_map(|n| {
        let probs = proptest::collection::vec(safe_prob(), n..=n);
        let weights = proptest::collection::vec(-2.0f64..2.0, n..=n);
        (probs, weights)
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        ..ProptestConfig::default()
    })]

    /// Output is in `[0, 1]` for any (probs, weights, alpha).
    #[test]
    fn fuse_output_in_unit_interval(
        (probs, weights) in arb_probs_weights(),
        alpha in 0.0f64..=1.0,
    ) {
        let mut f = LearnedFusion::new(probs.len(), alpha);
        f.weights = weights;
        let p = f.fuse(&probs).expect("generated shapes match");
        prop_assert!((0.0..=1.0).contains(&p), "fuse returned {p}");
    }

    /// `n = 1` is the identity: `fuse([p]) == p`.
    #[test]
    fn n1_identity(p in safe_prob(), alpha in 0.0f64..=1.0) {
        let f = LearnedFusion::new(1, alpha);
        let got = f.fuse(&[p]).expect("generated shapes match");
        prop_assert!((got - p).abs() < 1e-12, "fuse([{p}]) = {got}");
    }

    /// Equal raw weights (any value) collapse to the uniform-weight
    /// log-odds. At `alpha = 0` this equals scale-neutral mean
    /// log-odds: `sigmoid(mean(logit p_i))`.
    #[test]
    fn equal_weights_at_alpha_zero_match_mean_log_odds(
        probs in proptest::collection::vec(safe_prob(), 2..=5),
        w in -2.0f64..2.0,
    ) {
        let mut f = LearnedFusion::new(probs.len(), 0.0);
        f.weights = vec![w; probs.len()];
        let got = f.fuse(&probs).expect("generated shapes match");
        let n = probs.len() as f64;
        let expected = sigmoid(probs.iter().map(|&p| logit(p)).sum::<f64>() / n);
        prop_assert!(
            (got - expected).abs() < 1e-9,
            "equal-weight fuse = {got} != mean log-odds = {expected}",
        );
    }

    /// Simultaneous permutation of `(probs, weights)` does not change
    /// the fused output: weighted log-odds is symmetric in pairs.
    #[test]
    fn fuse_pair_permutation_invariant(
        (probs, weights) in arb_probs_weights(),
        alpha in 0.0f64..=1.0,
    ) {
        let mut f = LearnedFusion::new(probs.len(), alpha);
        f.weights = weights.clone();
        let direct = f.fuse(&probs).expect("generated shapes match");

        let mut order: Vec<usize> = (0..probs.len()).collect();
        order.reverse();
        let permuted_probs: Vec<f64> = order.iter().map(|&i| probs[i]).collect();
        let permuted_weights: Vec<f64> = order.iter().map(|&i| weights[i]).collect();

        let mut g = LearnedFusion::new(permuted_probs.len(), alpha);
        g.weights = permuted_weights;
        let permuted = g
            .fuse(&permuted_probs)
            .expect("generated shapes match");
        prop_assert!(
            (direct - permuted).abs() < 1e-9,
            "fuse changed under (prob, weight) permutation: {direct} vs {permuted}",
        );
    }
}

/// Concrete edge cases.
#[test]
fn empty_input_returns_error() {
    let f = LearnedFusion::new(0, 0.5);
    assert_eq!(
        f.fuse(&[]),
        Err("learned fusion requires at least one signal")
    );
}

#[test]
fn fuse_single_prob_returns_it() {
    let f = LearnedFusion::new(1, 0.5);
    assert!((f.fuse(&[0.7]).expect("matching model arity") - 0.7).abs() < 1e-12);
}

#[test]
fn mismatched_model_arity_returns_error() {
    let f = LearnedFusion::new(2, 0.5);
    assert_eq!(
        f.fuse(&[0.7]),
        Err("learned fusion signal count does not match the model")
    );
}

#[test]
fn invalid_learned_alpha_returns_error() {
    for alpha in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
        let fusion = LearnedFusion::new(2, alpha);
        assert_eq!(
            fusion.fuse(&[0.7, 0.6]),
            Err("learned fusion alpha must be finite and in [0, 1]")
        );
    }
}
