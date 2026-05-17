//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for metric-wrapper cases in `test_calibration`.

use uqa_scoring::CalibrationMetrics;

fn approx_eq(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "expected {a} ~= {b}");
}

#[test]
fn ece_perfect_calibration() {
    approx_eq(
        CalibrationMetrics::ece(&[0.0, 0.0, 1.0, 1.0], &[0, 0, 1, 1], 10),
        0.0,
        1e-6,
    );
}

#[test]
fn ece_imperfect_calibration() {
    assert!(CalibrationMetrics::ece(&[0.9, 0.9, 0.1, 0.1], &[0, 0, 1, 1], 10) > 0.0);
}

#[test]
fn ece_returns_float() {
    let result = CalibrationMetrics::ece(&[0.5, 0.5, 0.5, 0.5], &[0, 1, 0, 1], 10);
    assert!(result.is_finite());
}

#[test]
fn brier_perfect_predictions() {
    approx_eq(
        CalibrationMetrics::brier(&[0.0, 0.0, 1.0, 1.0], &[0, 0, 1, 1]),
        0.0,
        1e-6,
    );
}

#[test]
fn brier_worst_predictions() {
    approx_eq(
        CalibrationMetrics::brier(&[1.0, 1.0, 0.0, 0.0], &[0, 0, 1, 1]),
        1.0,
        1e-6,
    );
}

#[test]
fn brier_uniform_predictions() {
    approx_eq(
        CalibrationMetrics::brier(&[0.5, 0.5, 0.5, 0.5], &[0, 1, 0, 1]),
        0.25,
        1e-6,
    );
}

#[test]
fn brier_returns_float() {
    assert!(CalibrationMetrics::brier(&[0.5], &[1]).is_finite());
}

#[test]
fn report_returns_struct() {
    let report = CalibrationMetrics::report(&[0.1, 0.4, 0.6, 0.9], &[0, 0, 1, 1], 10);
    assert!(report.ece >= 0.0);
}

#[test]
fn report_contains_metrics() {
    let report = CalibrationMetrics::report(&[0.1, 0.4, 0.6, 0.9], &[0, 0, 1, 1], 10);
    assert!(report.ece >= 0.0);
    assert!(report.brier >= 0.0);
}

#[test]
fn reliability_diagram_returns_list() {
    let diagram = CalibrationMetrics::reliability_diagram(
        &[0.1, 0.2, 0.3, 0.7, 0.8, 0.9],
        &[0, 0, 0, 1, 1, 1],
        10,
    );
    assert!(!diagram.is_empty());
}

#[test]
fn reliability_diagram_tuple_structure() {
    let diagram = CalibrationMetrics::reliability_diagram(
        &[0.1, 0.2, 0.3, 0.7, 0.8, 0.9],
        &[0, 0, 0, 1, 1, 1],
        5,
    );
    for bin in diagram {
        assert!(bin.avg_predicted.is_finite());
        assert!(bin.avg_actual.is_finite());
    }
}

#[test]
fn reliability_diagram_n_bins() {
    let diagram =
        CalibrationMetrics::reliability_diagram(&[0.1, 0.3, 0.5, 0.7, 0.9], &[0, 0, 1, 1, 1], 5);
    assert!(diagram.len() <= 5);
}

#[test]
fn ece_with_many_bins() {
    let ece = CalibrationMetrics::ece(&[0.1, 0.3, 0.5, 0.7, 0.9], &[0, 0, 1, 1, 1], 20);
    assert!(ece >= 0.0);
}

#[test]
fn brier_score_range() {
    let score = CalibrationMetrics::brier(&[0.3, 0.7, 0.2, 0.8], &[0, 1, 0, 1]);
    assert!((0.0..=1.0).contains(&score));
}
