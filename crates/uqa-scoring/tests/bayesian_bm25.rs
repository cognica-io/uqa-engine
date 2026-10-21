//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy composite-prior score decomposition property tests.
//!
//! These tests preserve the historical arithmetic without claiming that its dependent inputs form a calibrated Bayesian posterior. Each probability-space update clamps its result, so the independent log-odds reference must retain the same stage boundaries within `1e-9`:
//!
//! ```text
//! C(p) = clamp(p, epsilon, 1 - epsilon)
//! p1 = C(sigmoid(logit L(s) + logit p_prior(f, n_hat)))
//! P = p1                                               // no base rate
//! P = C(sigmoid(logit p1 + logit b_r))                  // with base rate
//! ```
//!
//! This file pins:
//! - the decomposition equivalence to within `1e-9` for both forms
//! - prior bounds: `composite_prior in [0.1, 0.9]`,
//!   `tf_prior in [0.2, 0.9]`, `norm_prior in [0.3, 0.9]`
//! - `score_signal` lives in `(0, 1)` and is monotone in `score`
//! - `combined_score` is sign-preserving in the log-odds direction

use proptest::prelude::*;
use uqa_scoring::prob::clamp_prob;
use uqa_scoring::{logit, sigmoid, LegacyCompositePriorTransform};

fn log_odds_update(signal: f64, prior: f64) -> f64 {
    clamp_prob(sigmoid(logit(signal) + logit(prior)))
}

#[test]
fn clipped_first_update_is_preserved_before_the_base_rate() {
    // Independent 80-digit decimal evaluations using the exact f64 clamp bounds.
    for (signal, prior, base_rate, expected) in [
        (1e-8, 0.001, 0.95, 1.899_999_996_579_998_2e-9),
        (1.0 - 1e-8, 0.999, 0.05, 0.999_999_998_099_999_8),
    ] {
        let actual = LegacyCompositePriorTransform::combined_score(signal, prior, Some(base_rate));
        assert!((actual - expected).abs() < 1e-15, "{actual} != {expected}");
        let unclipped = sigmoid(logit(signal) + logit(prior) + logit(base_rate));
        assert!((actual - unclipped).abs() > 1e-9);
        let first = log_odds_update(signal, prior);
        assert!((actual - log_odds_update(first, base_rate)).abs() < 1e-15);
    }
}

#[test]
fn captured_saturated_score_matches_the_staged_log_odds_reference() {
    let transform = LegacyCompositePriorTransform::new(
        2.859_533_698_900_037,
        -1.663_713_539_871_294_1,
        Some(0.05),
    )
    .unwrap();
    let signal = transform.score_signal(4.803_593_465_550_421_5);
    let prior = 0.998_799_126_052_224_8;
    let actual = LegacyCompositePriorTransform::combined_score(signal, prior, Some(0.05));
    assert!((actual - 0.999_999_998_099_999_8).abs() < 1e-15);
    let expected = log_odds_update(log_odds_update(signal, prior), 0.05);
    assert!((actual - expected).abs() < 1e-15);
    let unclipped = sigmoid(logit(signal) + logit(prior) + logit(0.05));
    assert!((actual - unclipped).abs() > 1e-9);
}

/// `safe_prob` keeps logits finite.
fn safe_prob() -> impl Strategy<Value = f64> {
    1e-6f64..(1.0 - 1e-6)
}

/// Build a transform from random `(alpha, beta, base_rate?)`.
fn arb_transform() -> impl Strategy<Value = LegacyCompositePriorTransform> {
    (
        0.1f64..3.0,  // alpha
        -2.0f64..2.0, // beta
        proptest::option::of(0.05f64..0.95),
    )
        .prop_map(|(a, b, br)| LegacyCompositePriorTransform::new(a, b, br).unwrap())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// Decomposition equivalence without a base-rate bias:
    /// the two-update probability-space implementation must equal
    /// `clamp_prob(sigmoid(logit(L) + logit(prior)))` within `1e-9`.
    #[test]
    fn combined_score_decomposes_without_base_rate(
        score in -5.0f64..5.0,
        prior in safe_prob(),
        alpha in 0.1f64..3.0,
        beta in -2.0f64..2.0,
    ) {
        let tx = LegacyCompositePriorTransform::new(alpha, beta, None).unwrap();
        let l = tx.score_signal(score);
        // Avoid saturated score signals that push logit() out of range.
        prop_assume!(l > 1e-12 && l < 1.0 - 1e-12);

        let direct = LegacyCompositePriorTransform::combined_score(l, prior, None);
        let closed = log_odds_update(l, prior);
        prop_assert!(
            (direct - closed).abs() < 1e-9,
            "no-base-rate decomposition broke: direct={direct}, closed={closed}",
        );
    }

    /// Decomposition equivalence with a base-rate bias:
    /// the two probability-space updates must equal the staged log-odds
    /// reference, including the intermediate probability clamp.
    #[test]
    fn combined_score_decomposes_with_base_rate(
        score in -5.0f64..5.0,
        prior in safe_prob(),
        base_rate in 0.05f64..0.95,
        alpha in 0.1f64..3.0,
        beta in -2.0f64..2.0,
    ) {
        let tx = LegacyCompositePriorTransform::new(alpha, beta, Some(base_rate)).unwrap();
        let l = tx.score_signal(score);
        prop_assume!(l > 1e-12 && l < 1.0 - 1e-12);

        let direct = LegacyCompositePriorTransform::combined_score(l, prior, Some(base_rate));
        let closed = log_odds_update(log_odds_update(l, prior), base_rate);
        prop_assert!(
            (direct - closed).abs() < 1e-9,
            "with-base-rate decomposition broke: direct={}, closed={}, br={}",
            direct,
            closed,
            base_rate,
        );
    }

    /// `tf_prior` is monotone non-decreasing in tf and lives in [0.2, 0.9].
    #[test]
    fn tf_prior_bounds_and_monotonicity(tf in 0.0f64..50.0, delta in 0.0f64..50.0) {
        let p1 = LegacyCompositePriorTransform::tf_prior(tf);
        let p2 = LegacyCompositePriorTransform::tf_prior(tf + delta);
        prop_assert!(
            (0.2 - 1e-12..=0.9 + 1e-12).contains(&p1),
            "tf_prior({tf}) = {p1} out of bounds",
        );
        prop_assert!(p2 + 1e-12 >= p1, "tf_prior not monotone: {p1} -> {p2}");
    }

    /// `norm_prior` peaks at `0.9` for `r = 0.5` and floors at `0.3`
    /// outside `[0, 1]`.
    #[test]
    fn norm_prior_bounds(r in -1.0f64..2.0) {
        let p = LegacyCompositePriorTransform::norm_prior(r);
        prop_assert!(
            (0.3 - 1e-12..=0.9 + 1e-12).contains(&p),
            "norm_prior({r}) = {p} out of bounds",
        );
    }

    /// `composite_prior` lives in `[0.1, 0.9]` for any input.
    #[test]
    fn composite_prior_bounded(tf in 0.0f64..50.0, r in 0.0f64..2.0) {
        let p = LegacyCompositePriorTransform::composite_prior(tf, r);
        prop_assert!(
            (0.1 - 1e-12..=0.9 + 1e-12).contains(&p),
            "composite_prior({tf}, {r}) = {p} out of bounds",
        );
    }

    /// The bounded score signal is in `(0, 1)` and increases in `score`.
    #[test]
    fn score_signal_is_monotone_in_score(
        score in -3.0f64..3.0,
        delta in 1e-3f64..1.0,
        tx in arb_transform(),
    ) {
        let lo = tx.score_signal(score);
        let hi = tx.score_signal(score + delta);
        prop_assert!(lo > 0.0 && lo < 1.0, "lo {lo} out of range");
        prop_assert!(hi > 0.0 && hi < 1.0, "hi {hi} out of range");
        prop_assert!(hi + 1e-15 > lo, "score_signal not monotone: {lo} -> {hi}");
    }

    /// Sign of `logit(combined_score)` matches the sign of
    /// `logit(L) + logit(prior) [+ logit(base_rate)]`. Essentially
    /// the same statement as the decomposition, but checked on the
    /// log-odds side directly.
    #[test]
    fn combined_score_sign_matches_log_odds(
        score in -5.0f64..5.0,
        prior in safe_prob(),
        base_rate in proptest::option::of(0.05f64..0.95),
        alpha in 0.1f64..3.0,
        beta in -2.0f64..2.0,
    ) {
        let tx = LegacyCompositePriorTransform::new(alpha, beta, base_rate).unwrap();
        let l = tx.score_signal(score);
        prop_assume!(l > 1e-12 && l < 1.0 - 1e-12);

        let post = LegacyCompositePriorTransform::combined_score(l, prior, base_rate);
        let post_lo = logit(post);
        let mut total = logit(l) + logit(prior);
        if let Some(br) = base_rate {
            total += logit(br);
        }
        if total.abs() > 1e-9 {
            prop_assert_eq!(
                post_lo.signum(),
                total.signum(),
                "combined_score sign diverged from log-odds sum",
            );
        }
    }
}
