//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Calibration-metric property tests (Paper 3 Section 11.3,
//! Paper 5 Section 8.3).
//!
//! Pins:
//! - `log_loss >= 0` for any input,
//! - `brier in [0, 1]` for any input,
//! - `ece in [0, 1]` for any input,
//! - all three return `0.0` on empty input,
//! - `log_loss` and `brier` are invariant under permutation of
//!   `(prob, label)` pairs (`ece` is not, since it bins by `prob`),
//! - `brier` equals the closed-form `mean((p - y)^2)`,
//! - perfect predictions zero `brier` and `log_loss`.

use proptest::prelude::*;
use uqa_scoring::CalibrationMetrics;

/// Probabilities clamped well away from 0 and 1 so that `log_loss`
/// stays well-conditioned.
fn safe_prob() -> impl Strategy<Value = f64> {
    1e-3f64..(1.0 - 1e-3)
}

/// `(probs, labels)` of matching length drawn from `1..=20`.
fn arb_probs_labels() -> impl Strategy<Value = (Vec<f64>, Vec<u8>)> {
    (1usize..=20).prop_flat_map(|n| {
        let probs = proptest::collection::vec(safe_prob(), n..=n);
        let labels = proptest::collection::vec(0u8..=1, n..=n);
        (probs, labels)
    })
}

/// `(probs, labels, seeds)` triple where all three vectors have the
/// same length. Used by the permutation-invariance test.
fn arb_probs_labels_seeds() -> impl Strategy<Value = (Vec<f64>, Vec<u8>, Vec<usize>)> {
    (1usize..=20).prop_flat_map(|n| {
        let probs = proptest::collection::vec(safe_prob(), n..=n);
        let labels = proptest::collection::vec(0u8..=1, n..=n);
        let seeds = proptest::collection::vec(0usize..1000, n..=n);
        (probs, labels, seeds)
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        ..ProptestConfig::default()
    })]

    /// `log_loss >= 0` for any input.
    #[test]
    fn log_loss_non_negative((probs, labels) in arb_probs_labels()) {
        let l = CalibrationMetrics::log_loss(&probs, &labels);
        prop_assert!(l >= 0.0, "log_loss returned {l}");
    }

    /// `brier in [0, 1]` for any input drawn from valid probabilities.
    #[test]
    fn brier_bounded((probs, labels) in arb_probs_labels()) {
        let b = CalibrationMetrics::brier(&probs, &labels);
        prop_assert!((0.0..=1.0).contains(&b), "brier = {b} out of [0, 1]");
    }

    /// `ece in [0, 1]` for any (input, n_bins).
    #[test]
    fn ece_bounded((probs, labels) in arb_probs_labels(), n_bins in 1usize..=20) {
        let e = CalibrationMetrics::ece(&probs, &labels, n_bins);
        prop_assert!((0.0..=1.0).contains(&e), "ece = {e} out of [0, 1]");
    }

    /// `log_loss` and `brier` are invariant under permutation of
    /// `(prob, label)` pairs. `ece` is not (it bins by `prob`).
    #[test]
    fn log_loss_and_brier_permutation_invariant(
        (probs, labels, seeds) in arb_probs_labels_seeds(),
    ) {
        let mut order: Vec<usize> = (0..probs.len()).collect();
        order.sort_by_key(|&i| seeds[i]);
        let permuted_probs: Vec<f64> = order.iter().map(|&i| probs[i]).collect();
        let permuted_labels: Vec<u8> = order.iter().map(|&i| labels[i]).collect();

        let l1 = CalibrationMetrics::log_loss(&probs, &labels);
        let l2 = CalibrationMetrics::log_loss(&permuted_probs, &permuted_labels);
        prop_assert!((l1 - l2).abs() < 1e-9, "log_loss changed under permutation: {l1} vs {l2}");

        let b1 = CalibrationMetrics::brier(&probs, &labels);
        let b2 = CalibrationMetrics::brier(&permuted_probs, &permuted_labels);
        prop_assert!((b1 - b2).abs() < 1e-9, "brier changed under permutation: {b1} vs {b2}");
    }

    /// `brier` equals `mean((p - y)^2)` exactly.
    #[test]
    fn brier_matches_closed_form((probs, labels) in arb_probs_labels()) {
        let direct = CalibrationMetrics::brier(&probs, &labels);
        let closed: f64 = probs
            .iter()
            .zip(&labels)
            .map(|(p, y)| (p - f64::from(*y)).powi(2))
            .sum::<f64>()
            / probs.len() as f64;
        prop_assert!(
            (direct - closed).abs() < 1e-12,
            "brier {direct} != closed-form {closed}",
        );
    }

    /// Perfect predictions zero out `brier` and `log_loss`.
    #[test]
    fn perfect_predictions_zero_loss(labels in proptest::collection::vec(0u8..=1, 1..=20)) {
        let probs: Vec<f64> = labels
            .iter()
            .map(|&y| if y == 1 { 1.0 - 1e-6 } else { 1e-6 })
            .collect();
        let b = CalibrationMetrics::brier(&probs, &labels);
        let l = CalibrationMetrics::log_loss(&probs, &labels);
        prop_assert!(b < 1e-9, "brier on perfect predictions = {b}");
        prop_assert!(l < 1e-3, "log_loss on perfect predictions = {l}");
    }
}

/// Empty inputs return 0 for all three metrics.
#[test]
fn empty_input_returns_zero() {
    assert_eq!(CalibrationMetrics::log_loss(&[], &[]), 0.0);
    assert_eq!(CalibrationMetrics::brier(&[], &[]), 0.0);
    assert_eq!(CalibrationMetrics::ece(&[], &[], 10), 0.0);
}

/// `n_bins == 0` returns 0 for ECE (degenerate).
#[test]
fn ece_zero_bins_returns_zero() {
    assert_eq!(CalibrationMetrics::ece(&[0.5; 10], &[0u8; 10], 0), 0.0);
}
