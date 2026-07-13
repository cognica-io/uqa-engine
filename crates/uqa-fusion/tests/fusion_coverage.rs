//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_fusion`.

use uqa_fusion::{LogOddsFusion, ProbabilisticBoolean};

fn approx_eq(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "expected {a} ~= {b}");
}

#[test]
fn softplus_gating_treats_every_match_as_positive_evidence() {
    let fusion = LogOddsFusion::new(0.0);
    for p in [0.1, 0.3, 0.5, 0.7, 0.9] {
        for n in [2, 3, 5, 10] {
            assert!(fusion.fuse(&vec![p; n]) > 0.5);
        }
    }
}

#[test]
fn softplus_gating_preserves_weak_match_ordering() {
    let fusion = LogOddsFusion::new(0.5);
    assert!(fusion.fuse(&[0.8, 0.7, 0.6, 0.9]) > 0.5);
    assert!(fusion.fuse(&[0.2, 0.3, 0.4, 0.1]) > 0.5);
    assert!(fusion.fuse(&[0.4, 0.3]) > fusion.fuse(&[0.2, 0.1]));
}

#[test]
fn sparse_absence_is_weaker_than_a_low_probability_match() {
    let fusion = LogOddsFusion::new(0.5);
    let absent = fusion.fuse_sparse(&[None, None]);
    let weak_match = fusion.fuse_sparse(&[Some(0.1), None]);
    assert_eq!(absent, 0.5);
    assert!(weak_match > absent);
}

#[test]
fn relevance_preservation() {
    let fusion = LogOddsFusion::new(0.5);
    assert!(fusion.fuse(&[0.51, 0.6, 0.7, 0.8, 0.9]) > 0.5);
}

#[test]
fn single_signal_identity() {
    let fusion = LogOddsFusion::new(0.5);
    for p in [0.1, 0.3, 0.5, 0.7, 0.9] {
        approx_eq(fusion.fuse(&[p]), p, 1e-12);
    }
}

#[test]
fn empty_returns_neutral() {
    assert_eq!(LogOddsFusion::new(0.5).fuse(&[]), 0.5);
}

#[test]
fn result_in_unit_interval() {
    let result = LogOddsFusion::new(0.5).fuse(&[0.01, 0.99, 0.5, 0.3, 0.8]);
    assert!((0.0..=1.0).contains(&result));
}

#[test]
fn weighted_fusion_basic() {
    let result = LogOddsFusion::new(0.5)
        .fuse_weighted(&[0.8, 0.2], &[0.5, 0.5])
        .unwrap();
    assert!((0.0..=1.0).contains(&result));
}

#[test]
fn weighted_fusion_empty_rejects_zero_sum_weights() {
    assert_eq!(
        LogOddsFusion::new(0.5).fuse_weighted(&[], &[]),
        Err("weights must sum to 1")
    );
}

#[test]
fn alpha_zero_is_mean() {
    let result = LogOddsFusion::new(0.0).fuse(&[0.8, 0.6]);
    assert!(result > 0.0);
    assert!(result < 1.0);
}

#[test]
fn alpha_one_is_sum() {
    let result = LogOddsFusion::new(1.0).fuse(&[0.8, 0.6]);
    assert!(result > 0.0);
    assert!(result < 1.0);
}

#[test]
fn gating_none_matches_default() {
    let default = LogOddsFusion::new(0.5);
    let none = LogOddsFusion::with_gating(0.5, None);
    approx_eq(
        default.fuse(&[0.8, 0.6, 0.7]),
        none.fuse(&[0.8, 0.6, 0.7]),
        1e-12,
    );
}

#[test]
fn gating_relu() {
    let result = LogOddsFusion::with_gating(0.5, Some("relu")).fuse(&[0.8, 0.6, 0.7]);
    assert!((0.0..=1.0).contains(&result));
}

#[test]
fn gating_swish() {
    let result = LogOddsFusion::with_gating(0.5, Some("swish")).fuse(&[0.8, 0.6, 0.7]);
    assert!((0.0..=1.0).contains(&result));
}

#[test]
fn prob_and_bounds() {
    let result = ProbabilisticBoolean::prob_and(&[0.9, 0.8, 0.7, 0.6]);
    assert!((0.0..=1.0).contains(&result));
}

#[test]
fn prob_or_bounds() {
    let result = ProbabilisticBoolean::prob_or(&[0.1, 0.2, 0.3, 0.4]);
    assert!((0.0..=1.0).contains(&result));
}

#[test]
fn prob_and_less_than_min() {
    let probs = [0.9, 0.8, 0.7];
    let result = ProbabilisticBoolean::prob_and(&probs);
    assert!(result <= probs.into_iter().fold(f64::INFINITY, f64::min) + 1e-10);
}

#[test]
fn prob_or_greater_than_max() {
    let probs = [0.1, 0.2, 0.3];
    let result = ProbabilisticBoolean::prob_or(&probs);
    assert!(result >= probs.into_iter().fold(f64::NEG_INFINITY, f64::max) - 1e-10);
}

#[test]
fn prob_and_single() {
    approx_eq(ProbabilisticBoolean::prob_and(&[0.7]), 0.7, 1e-12);
}

#[test]
fn prob_or_single() {
    approx_eq(ProbabilisticBoolean::prob_or(&[0.3]), 0.3, 1e-12);
}

#[test]
fn prob_not() {
    approx_eq(ProbabilisticBoolean::prob_not(0.3), 0.7, 1e-12);
    approx_eq(ProbabilisticBoolean::prob_not(1.0), 0.0, 1e-9);
    approx_eq(ProbabilisticBoolean::prob_not(0.0), 1.0, 1e-9);
}

#[test]
fn prob_and_with_certainty() {
    approx_eq(ProbabilisticBoolean::prob_and(&[1.0, 0.5, 0.8]), 0.4, 1e-9);
}

#[test]
fn prob_or_with_certainty() {
    approx_eq(ProbabilisticBoolean::prob_or(&[1.0, 0.5, 0.3]), 1.0, 1e-9);
}

#[test]
fn de_morgan_consistency() {
    let probs = [0.6, 0.7, 0.8];
    let not_and = ProbabilisticBoolean::prob_not(ProbabilisticBoolean::prob_and(&probs));
    let or_not = ProbabilisticBoolean::prob_or(
        &probs
            .into_iter()
            .map(ProbabilisticBoolean::prob_not)
            .collect::<Vec<_>>(),
    );
    approx_eq(not_and, or_not, 1e-12);
}

#[test]
fn prob_and_near_zero() {
    let result = ProbabilisticBoolean::prob_and(&[0.001, 0.002, 0.003]);
    assert!(result >= 0.0);
    approx_eq(result, 0.001 * 0.002 * 0.003, 1e-18);
}
