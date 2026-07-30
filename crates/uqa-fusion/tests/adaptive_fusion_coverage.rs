//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for fusion-level cases in `test_adaptive_fusion`.

use uqa_fusion::{AdaptiveLogOddsFusion, LogOddsFusion, SignalQuality};

fn approx_eq(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "expected {a} ~= {b}");
}

#[test]
fn signal_quality_creation() {
    let sq = SignalQuality {
        coverage_ratio: 0.8,
        score_variance: 0.05,
        calibration_error: 0.1,
    };
    approx_eq(sq.coverage_ratio, 0.8);
    approx_eq(sq.score_variance, 0.05);
    approx_eq(sq.calibration_error, 0.1);
}

#[test]
fn compute_signal_alpha_high_quality() {
    let fusion = AdaptiveLogOddsFusion::new(0.5);
    let sq = SignalQuality {
        coverage_ratio: 1.0,
        score_variance: 0.0,
        calibration_error: 0.0,
    };
    approx_eq(fusion.compute_signal_alpha(sq), 0.5);
}

#[test]
fn compute_signal_alpha_low_quality() {
    let fusion = AdaptiveLogOddsFusion::new(0.5);
    let sq = SignalQuality {
        coverage_ratio: 0.1,
        score_variance: 5.0,
        calibration_error: 0.4,
    };
    approx_eq(fusion.compute_signal_alpha(sq), 0.01);
}

#[test]
fn compute_signal_alpha_clamping() {
    let fusion = AdaptiveLogOddsFusion::new(0.5);
    let low = SignalQuality {
        coverage_ratio: 0.0,
        score_variance: 0.0,
        calibration_error: 0.0,
    };
    approx_eq(fusion.compute_signal_alpha(low), 0.01);

    let fusion_high = AdaptiveLogOddsFusion::new(5.0);
    let high = SignalQuality {
        coverage_ratio: 1.0,
        score_variance: 0.0,
        calibration_error: 0.0,
    };
    approx_eq(fusion_high.compute_signal_alpha(high), 1.0);
}

#[test]
fn adaptive_fuse_single_signal() {
    let fusion = AdaptiveLogOddsFusion::new(0.5);
    let sq = SignalQuality {
        coverage_ratio: 1.0,
        score_variance: 0.0,
        calibration_error: 0.0,
    };
    approx_eq(fusion.fuse_adaptive(&[0.8], &[sq]).unwrap(), 0.8);
}

#[test]
fn adaptive_fuse_uniform_quality() {
    let adaptive = AdaptiveLogOddsFusion::new(0.5);
    let standard = LogOddsFusion::new(0.5).unwrap();
    let probs = [0.7, 0.8, 0.6];
    let sq = SignalQuality {
        coverage_ratio: 1.0,
        score_variance: 0.0,
        calibration_error: 0.0,
    };
    let qualities = [sq, sq, sq];

    assert!(adaptive.fuse_adaptive(&probs, &qualities).unwrap() > 0.5);
    assert!(standard.fuse(&probs) > 0.5);
}

#[test]
fn adaptive_fuse_mixed_quality() {
    let fusion = AdaptiveLogOddsFusion::new(0.5);
    let high_q = SignalQuality {
        coverage_ratio: 1.0,
        score_variance: 0.0,
        calibration_error: 0.0,
    };
    let low_q = SignalQuality {
        coverage_ratio: 0.1,
        score_variance: 5.0,
        calibration_error: 0.4,
    };

    let result_high_first = fusion.fuse_adaptive(&[0.9, 0.1], &[high_q, low_q]).unwrap();
    let result_low_first = fusion.fuse_adaptive(&[0.1, 0.9], &[high_q, low_q]).unwrap();
    assert!(result_high_first > result_low_first);
}
