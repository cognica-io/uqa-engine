//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Property tests for the probability primitives in
//! `uqa_scoring::prob`.
//!
//! Pins:
//! - sigmoid <-> logit duality on `(epsilon, 1 - epsilon)`,
//! - `cosine_to_probability` is monotone non-decreasing in score and
//!   maps `[-1, 1]` into `[epsilon, 1 - epsilon]`,
//! - `prob_and` and `prob_or` are commutative,
//! - `prob_and` is associative,
//! - De Morgan duality: `prob_or(p_i) == 1 - prob_and(1 - p_i)` (within
//!   the floating-point envelope `clamp_prob` enforces).

use proptest::prelude::*;
use uqa_scoring::{
    cosine_to_probability, logit, prob_and, prob_not, prob_or, sigmoid, PROB_EPSILON,
};

/// Probabilities clamped well away from 0 and 1 to keep `logit`
/// finite and the De Morgan dual numerically stable.
fn safe_prob() -> impl Strategy<Value = f64> {
    1e-3f64..(1.0 - 1e-3)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// `sigmoid(logit(p)) == p` and `logit(sigmoid(x)) == x` within
    /// 1e-9 on the safe interior.
    #[test]
    fn sigmoid_logit_round_trip(p in safe_prob(), x in -10.0f64..10.0) {
        prop_assert!(
            (sigmoid(logit(p)) - p).abs() < 1e-9,
            "sigmoid(logit({p})) = {} != p",
            sigmoid(logit(p)),
        );
        prop_assert!(
            (logit(sigmoid(x)) - x).abs() < 1e-9,
            "logit(sigmoid({x})) = {} != x",
            logit(sigmoid(x)),
        );
    }

    /// `cosine_to_probability` is monotone non-decreasing in the
    /// cosine score.
    #[test]
    fn cosine_to_probability_monotone(a in -1.0f64..1.0, delta in 0.0f64..0.5) {
        let lo = cosine_to_probability(a);
        let hi = cosine_to_probability((a + delta).min(1.0));
        prop_assert!(hi + 1e-12 >= lo, "{lo} -> {hi}");
        prop_assert!(
            (PROB_EPSILON..=1.0 - PROB_EPSILON).contains(&lo),
            "cosine_to_probability out of clamp: {lo}",
        );
    }

    /// `prob_and` is invariant under permutation of its inputs.
    #[test]
    fn prob_and_commutative(probs in proptest::collection::vec(safe_prob(), 0..6)) {
        let mut reversed = probs.clone();
        reversed.reverse();
        prop_assert!(
            (prob_and(&probs) - prob_and(&reversed)).abs() < 1e-9,
            "prob_and not commutative on {probs:?}",
        );
    }

    /// `prob_or` is invariant under permutation of its inputs.
    #[test]
    fn prob_or_commutative(probs in proptest::collection::vec(safe_prob(), 0..6)) {
        let mut reversed = probs.clone();
        reversed.reverse();
        prop_assert!(
            (prob_or(&probs) - prob_or(&reversed)).abs() < 1e-9,
            "prob_or not commutative on {probs:?}",
        );
    }

    /// `prob_and` is associative: `and(a, and(b, c)) == and(and(a, b), c)`.
    /// Implementation-wise both reduce to `exp(sum(ln(p_i)))`, so any
    /// regression that introduces ordering sensitivity will surface
    /// here.
    #[test]
    fn prob_and_associative(a in safe_prob(), b in safe_prob(), c in safe_prob()) {
        let left = prob_and(&[a, prob_and(&[b, c])]);
        let right = prob_and(&[prob_and(&[a, b]), c]);
        let flat = prob_and(&[a, b, c]);
        prop_assert!((left - flat).abs() < 1e-9, "and(a, and(b, c)) = {left} != flat {flat}");
        prop_assert!((right - flat).abs() < 1e-9, "and(and(a, b), c) = {right} != flat {flat}");
    }

    /// De Morgan: `prob_or(p_i) == 1 - prob_and(1 - p_i)` within the
    /// envelope that `clamp_prob` enforces. Both sides clamp inputs
    /// before log/exp, so equality is exact only away from the
    /// boundary.
    #[test]
    fn de_morgan_or_via_and(probs in proptest::collection::vec(safe_prob(), 1..5)) {
        let direct = prob_or(&probs);
        let dual = {
            let inverted: Vec<f64> = probs.iter().map(|p| prob_not(*p)).collect();
            1.0 - prob_and(&inverted)
        };
        prop_assert!(
            (direct - dual).abs() < 1e-9,
            "prob_or {direct} != 1 - prob_and(1-p_i) {dual} on {probs:?}",
        );
    }

    /// `prob_and` of a single-element list is that element (within the
    /// floor of `clamp_prob`).
    #[test]
    fn prob_and_single_element(p in safe_prob()) {
        let got = prob_and(&[p]);
        prop_assert!((got - p).abs() < 1e-9, "and([{p}]) = {got}");
    }

    /// `prob_or` of a single-element list is that element.
    #[test]
    fn prob_or_single_element(p in safe_prob()) {
        let got = prob_or(&[p]);
        prop_assert!((got - p).abs() < 1e-9, "or([{p}]) = {got}");
    }

    /// `prob_not(prob_not(p)) == p` within the clamp floor.
    #[test]
    fn prob_not_involution(p in safe_prob()) {
        let got = prob_not(prob_not(p));
        prop_assert!((got - p).abs() < 1e-9, "not(not({p})) = {got}");
    }
}

/// Boundary cases worth pinning concretely.
#[test]
fn empty_inputs_have_neutral_elements() {
    assert_eq!(prob_and(&[]), 1.0);
    assert_eq!(prob_or(&[]), 0.0);
}

#[test]
fn cosine_at_extremes_clamps() {
    assert!((cosine_to_probability(1.0) - (1.0 - PROB_EPSILON)).abs() < 1e-12);
    assert!((cosine_to_probability(-1.0) - PROB_EPSILON).abs() < 1e-12);
    assert!((cosine_to_probability(0.0) - 0.5).abs() < 1e-12);
}
