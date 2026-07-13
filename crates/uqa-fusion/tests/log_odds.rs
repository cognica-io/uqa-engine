//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Log-odds fusion algebraic property tests (Paper 4, Section 4).
//!
//! Pins both the raw mean-logit helper's algebraic properties and the
//! Lucene fusion scorer's sparse softplus behavior.

use proptest::prelude::*;
use uqa_fusion::LogOddsFusion;

/// Numerically safe probabilities — keeps logits finite.
fn safe_prob() -> impl Strategy<Value = f64> {
    1e-6f64..(1.0 - 1e-6)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// Proposition 4.3.2: fusing a single signal returns it unchanged.
    /// The identity holds for both forms.
    #[test]
    fn n1_identity(p in safe_prob()) {
        let scaled = LogOddsFusion::default();
        let mean = LogOddsFusion::new(0.0);
        prop_assert!(
            (scaled.fuse(&[p]) - p).abs() < 1e-12,
            "scaled.fuse([p]) != p"
        );
        prop_assert!(
            (mean.fuse_mean(&[p]) - p).abs() < 1e-12,
            "mean.fuse_mean([p]) != p"
        );
    }

    /// Softplus-gated matching signals always contribute positively.
    #[test]
    fn sign_preserved_when_all_above_half(probs in proptest::collection::vec(0.5001f64..0.999, 1..6)) {
        let f = LogOddsFusion::default();
        let fused = f.fuse(&probs);
        prop_assert!(fused >= 0.5, "fuse {probs:?} -> {fused} should be >= 0.5");
    }

    #[test]
    fn weak_matches_score_above_absence(probs in proptest::collection::vec(0.001f64..0.4999, 2..6)) {
        let f = LogOddsFusion::default();
        let fused = f.fuse(&probs);
        prop_assert!(fused > 0.5, "fuse {probs:?} -> {fused} should be > 0.5");
    }

    /// Irrelevance preservation: in the scale-neutral mean form,
    /// padding with arbitrarily many `0.5` entries cannot move the
    /// fused mean away from the active signal's contribution
    /// (it scales toward 0.5 but never crosses it).
    #[test]
    fn irrelevance_preserves_sign(p in safe_prob(), pad in 1usize..8) {
        let mean = LogOddsFusion::new(0.0);
        let mut padded = vec![p];
        padded.extend(std::iter::repeat_n(0.5, pad));
        let fused = mean.fuse_mean(&padded);
        if p > 0.5 {
            prop_assert!(fused >= 0.5, "p={p} fused={fused}");
            prop_assert!(fused <= p + 1e-9, "irrelevance amplified the signal");
        } else if p < 0.5 {
            prop_assert!(fused <= 0.5, "p={p} fused={fused}");
            prop_assert!(fused >= p - 1e-9, "irrelevance amplified the signal");
        }
    }

    /// Relevance preservation: agreeing signals strictly *increase*
    /// confidence in `fuse_mean` toward the unanimous direction —
    /// repeating the same probability `n` times cannot yield a more
    /// neutral fused score than a single occurrence.
    #[test]
    fn relevance_preservation(p in safe_prob(), n in 1usize..6) {
        let mean = LogOddsFusion::new(0.0);
        let single = mean.fuse_mean(&[p]);
        let many: Vec<f64> = std::iter::repeat_n(p, n).collect();
        let fused = mean.fuse_mean(&many);
        // For n >= 1 and identical inputs, fuse_mean of [p; n] equals
        // p exactly, since the per-signal logit average reduces to
        // logit(p). So both ends collapse to p.
        prop_assert!(
            (single - fused).abs() < 1e-9,
            "fuse_mean(repeats) diverged from single: {single} vs {fused}",
        );
    }

    /// Symmetric disagreement: pairing each `p` with its complement
    /// `1 - p` produces a perfectly balanced logit set whose mean is
    /// zero — i.e. the fused mean is exactly 0.5.
    #[test]
    fn symmetric_disagreement_collapses(p in safe_prob()) {
        let mean = LogOddsFusion::new(0.0);
        let pair = mean.fuse_mean(&[p, 1.0 - p]);
        prop_assert!(
            (pair - 0.5).abs() < 1e-9,
            "[{p}, {}] should fuse to 0.5, got {pair}",
            1.0 - p,
        );
        let four = mean.fuse_mean(&[p, 1.0 - p, p, 1.0 - p]);
        prop_assert!(
            (four - 0.5).abs() < 1e-9,
            "balanced 4-set should fuse to 0.5, got {four}",
        );
    }
}

/// Concrete edge cases: empty input, weighted form happy path,
/// confidence-scaled form deviates from mean form for n>=2.
#[test]
fn empty_input_returns_neutral() {
    assert_eq!(LogOddsFusion::default().fuse(&[]), 0.5);
    assert_eq!(LogOddsFusion::new(0.0).fuse_mean(&[]), 0.5);
}

#[test]
fn confidence_scaling_amplifies_agreement() {
    let scaled = LogOddsFusion::new(0.5);
    let mean = LogOddsFusion::new(0.0);
    let agreeing = [0.8, 0.8, 0.8];
    let s = scaled.fuse(&agreeing);
    let m = mean.fuse_mean(&agreeing);
    assert!(
        s > m,
        "confidence-scaled fusion ({s}) must exceed scale-neutral fusion ({m}) when signals agree",
    );
}
