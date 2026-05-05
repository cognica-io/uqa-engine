//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bayesian BM25 posterior decomposition property tests
//! (Master plan Section 2.3 / Paper 3 Section 4).
//!
//! The probability-space two-update implementation must agree with the
//! closed-form log-odds decomposition within `1e-9`:
//!
//! ```text
//! logit P(R=1 | s, f, n_hat)
//!   = logit L(s) + logit p_prior(f, n_hat)               // no base rate
//!   = logit L(s) + logit p_prior(f, n_hat) + logit b_r   // with base rate
//! ```
//!
//! This file pins:
//! - the decomposition equivalence to within `1e-9` for both forms
//! - prior bounds: `composite_prior in [0.1, 0.9]`,
//!   `tf_prior in [0.2, 0.9]`, `norm_prior in [0.3, 0.9]`
//! - likelihood lives in `(0, 1)` and is monotone in `score`
//! - posterior is sign-preserving in the log-odds direction

use proptest::prelude::*;
use uqa_scoring::{logit, sigmoid, BayesianProbabilityTransform};

/// `safe_prob` keeps logits finite.
fn safe_prob() -> impl Strategy<Value = f64> {
    1e-6f64..(1.0 - 1e-6)
}

/// Build a transform from random `(alpha, beta, base_rate?)`.
fn arb_transform() -> impl Strategy<Value = BayesianProbabilityTransform> {
    (
        0.1f64..3.0,  // alpha
        -2.0f64..2.0, // beta
        proptest::option::of(0.05f64..0.95),
    )
        .prop_map(|(a, b, br)| BayesianProbabilityTransform::new(a, b, br))
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// Posterior decomposition equivalence (no base rate):
    /// the two-update probability-space implementation must equal
    /// `sigmoid(logit(L) + logit(prior))` within `1e-9`.
    #[test]
    fn posterior_decomposes_without_base_rate(
        score in -5.0f64..5.0,
        prior in safe_prob(),
        alpha in 0.1f64..3.0,
        beta in -2.0f64..2.0,
    ) {
        let tx = BayesianProbabilityTransform::new(alpha, beta, None);
        let l = tx.likelihood(score);
        // Avoid degenerate likelihoods that push logit() out of range.
        prop_assume!(l > 1e-12 && l < 1.0 - 1e-12);

        let direct = BayesianProbabilityTransform::posterior(l, prior, None);
        let closed = sigmoid(logit(l) + logit(prior));
        prop_assert!(
            (direct - closed).abs() < 1e-9,
            "no-base-rate decomposition broke: direct={direct}, closed={closed}",
        );
    }

    /// Posterior decomposition equivalence (with base rate):
    /// the two probability-space updates must equal the three-term
    /// log-odds sum.
    #[test]
    fn posterior_decomposes_with_base_rate(
        score in -5.0f64..5.0,
        prior in safe_prob(),
        base_rate in 0.05f64..0.95,
        alpha in 0.1f64..3.0,
        beta in -2.0f64..2.0,
    ) {
        let tx = BayesianProbabilityTransform::new(alpha, beta, Some(base_rate));
        let l = tx.likelihood(score);
        prop_assume!(l > 1e-12 && l < 1.0 - 1e-12);

        let direct = BayesianProbabilityTransform::posterior(l, prior, Some(base_rate));
        let closed = sigmoid(logit(l) + logit(prior) + logit(base_rate));
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
        let p1 = BayesianProbabilityTransform::tf_prior(tf);
        let p2 = BayesianProbabilityTransform::tf_prior(tf + delta);
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
        let p = BayesianProbabilityTransform::norm_prior(r);
        prop_assert!(
            (0.3 - 1e-12..=0.9 + 1e-12).contains(&p),
            "norm_prior({r}) = {p} out of bounds",
        );
    }

    /// `composite_prior` lives in `[0.1, 0.9]` for any input.
    #[test]
    fn composite_prior_bounded(tf in 0.0f64..50.0, r in 0.0f64..2.0) {
        let p = BayesianProbabilityTransform::composite_prior(tf, r);
        prop_assert!(
            (0.1 - 1e-12..=0.9 + 1e-12).contains(&p),
            "composite_prior({tf}, {r}) = {p} out of bounds",
        );
    }

    /// Likelihood is in `(0, 1)` and strictly increases in `score`.
    #[test]
    fn likelihood_monotone_in_score(
        score in -3.0f64..3.0,
        delta in 1e-3f64..1.0,
        tx in arb_transform(),
    ) {
        let lo = tx.likelihood(score);
        let hi = tx.likelihood(score + delta);
        prop_assert!(lo > 0.0 && lo < 1.0, "lo {lo} out of range");
        prop_assert!(hi > 0.0 && hi < 1.0, "hi {hi} out of range");
        prop_assert!(hi + 1e-15 > lo, "likelihood not monotone: {lo} -> {hi}");
    }

    /// Sign of `logit(posterior)` matches the sign of
    /// `logit(L) + logit(prior) [+ logit(base_rate)]`. Essentially
    /// the same statement as the decomposition, but checked on the
    /// log-odds side directly.
    #[test]
    fn posterior_sign_matches_log_odds(
        score in -5.0f64..5.0,
        prior in safe_prob(),
        base_rate in proptest::option::of(0.05f64..0.95),
        alpha in 0.1f64..3.0,
        beta in -2.0f64..2.0,
    ) {
        let tx = BayesianProbabilityTransform::new(alpha, beta, base_rate);
        let l = tx.likelihood(score);
        prop_assume!(l > 1e-12 && l < 1.0 - 1e-12);

        let post = BayesianProbabilityTransform::posterior(l, prior, base_rate);
        let post_lo = logit(post);
        let mut total = logit(l) + logit(prior);
        if let Some(br) = base_rate {
            total += logit(br);
        }
        if total.abs() > 1e-9 {
            prop_assert_eq!(
                post_lo.signum(),
                total.signum(),
                "posterior sign diverged from log-odds sum",
            );
        }
    }
}
