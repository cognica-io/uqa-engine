//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned calibration-model provenance, candidate-K drift, and held-out
//! regression-gate coverage.

use serde::Deserialize;
use uqa_scoring::{
    BootstrapConfig, HeldOutCalibrationGate, HeldOutCalibrationReport, ThresholdTransferReport,
    VectorCalibrationModel, VectorCalibrationProvenance, VectorCalibrationStabilityReport,
    VectorProbabilityTransform,
};

const FIXTURE_JSON: &str = include_str!("../../../tests/parity/vector_calibration_fixture.json");

#[derive(Deserialize)]
struct CalibrationFixture {
    version: u32,
    fixture_id: String,
    target_population: String,
    seed: u64,
    n_bins: usize,
    bootstrap_resamples: usize,
    confidence_level: f64,
    reference: ModelFixture,
    candidate_k_model: ModelFixture,
    shared_distance_probes: Vec<f64>,
    max_mean_absolute_k_drift: f64,
    max_absolute_k_drift: f64,
    validation: LabeledSplit,
    held_out: LabeledSplit,
    gate: HeldOutCalibrationGateFixture,
}

#[derive(Deserialize)]
struct ModelFixture {
    transform: VectorProbabilityTransform,
    provenance: VectorCalibrationProvenance,
}

#[derive(Deserialize)]
struct LabeledSplit {
    probabilities: Vec<f64>,
    labels: Vec<u8>,
}

#[derive(Deserialize)]
struct HeldOutCalibrationGateFixture {
    max_ece_upper: f64,
    max_brier_upper: f64,
    max_log_loss_upper: f64,
    min_transferred_f1: f64,
}

fn fixture() -> CalibrationFixture {
    let fixture: CalibrationFixture = serde_json::from_str(FIXTURE_JSON).unwrap();
    assert_eq!(fixture.version, 1);
    assert!(!fixture.fixture_id.is_empty());
    assert!(!fixture.target_population.is_empty());
    fixture
}

fn model(fixture: ModelFixture) -> VectorCalibrationModel {
    VectorCalibrationModel::new(fixture.transform, fixture.provenance).unwrap()
}

#[test]
fn model_json_keeps_provenance_and_rejects_a_different_runtime_k() {
    let fixture = fixture();
    let reference = model(fixture.reference);
    let json = reference.to_json().unwrap();
    let restored = VectorCalibrationModel::from_json(&json).unwrap();
    assert_eq!(restored, reference);

    let matching = reference.provenance().target.clone();
    assert!(restored.calibrate_one(0.2, &matching).is_ok());
    let mut wrong_k = matching;
    wrong_k.candidate_k += 1;
    let error = restored.calibrate_one(0.2, &wrong_k).unwrap_err();
    assert!(error.to_string().contains("target mismatch"), "{error}");
}

#[test]
fn candidate_k_probability_drift_is_measured_and_gated() {
    let fixture = fixture();
    let reference = model(fixture.reference);
    let candidate = model(fixture.candidate_k_model);
    let report = VectorCalibrationStabilityReport::compare(
        &reference,
        &candidate,
        &fixture.shared_distance_probes,
    )
    .unwrap();

    assert_eq!(report.reference_k, 4);
    assert_eq!(report.candidate_k, 8);
    assert!(report.mean_absolute_drift > 0.0);
    assert!(
        report.mean_absolute_drift <= fixture.max_mean_absolute_k_drift,
        "mean K drift {} exceeded fixture gate {}",
        report.mean_absolute_drift,
        fixture.max_mean_absolute_k_drift
    );
    assert!(
        report.max_absolute_drift <= fixture.max_absolute_k_drift,
        "max K drift {} exceeded fixture gate {}",
        report.max_absolute_drift,
        fixture.max_absolute_k_drift
    );
}

#[test]
fn held_out_confidence_and_transferred_threshold_pass_the_versioned_gate() {
    let fixture = fixture();
    let report = HeldOutCalibrationReport::evaluate(
        &fixture.held_out.probabilities,
        &fixture.held_out.labels,
        fixture.n_bins,
        BootstrapConfig {
            resamples: fixture.bootstrap_resamples,
            confidence_level: fixture.confidence_level,
            seed: fixture.seed,
        },
    )
    .unwrap();
    let repeated = HeldOutCalibrationReport::evaluate(
        &fixture.held_out.probabilities,
        &fixture.held_out.labels,
        fixture.n_bins,
        report.bootstrap,
    )
    .unwrap();
    assert_eq!(report, repeated, "bootstrap report must be seed-stable");
    assert_eq!(report.sample_count, fixture.held_out.labels.len());
    assert_eq!(report.point.bins.len(), fixture.n_bins);

    let transfer = ThresholdTransferReport::evaluate(
        &fixture.validation.probabilities,
        &fixture.validation.labels,
        &fixture.held_out.probabilities,
        &fixture.held_out.labels,
    )
    .unwrap();
    assert_eq!(transfer.threshold, 0.65);
    assert_eq!(transfer.validation.f1, 1.0);
    assert!((transfer.held_out.f1 - 22.0 / 23.0).abs() < 1e-12);

    HeldOutCalibrationGate {
        max_ece_upper: fixture.gate.max_ece_upper,
        max_brier_upper: fixture.gate.max_brier_upper,
        max_log_loss_upper: fixture.gate.max_log_loss_upper,
        min_transferred_f1: fixture.gate.min_transferred_f1,
    }
    .check(&report, &transfer)
    .unwrap();
}

#[test]
fn held_out_gate_fails_on_its_confidence_bound_not_only_the_point_estimate() {
    let fixture = fixture();
    let report = HeldOutCalibrationReport::evaluate(
        &fixture.held_out.probabilities,
        &fixture.held_out.labels,
        fixture.n_bins,
        BootstrapConfig {
            resamples: 64,
            confidence_level: 0.95,
            seed: fixture.seed,
        },
    )
    .unwrap();
    let transfer = ThresholdTransferReport::evaluate(
        &fixture.validation.probabilities,
        &fixture.validation.labels,
        &fixture.held_out.probabilities,
        &fixture.held_out.labels,
    )
    .unwrap();
    let error = HeldOutCalibrationGate {
        max_ece_upper: report.point.ece,
        max_brier_upper: 1.0,
        max_log_loss_upper: f64::MAX,
        min_transferred_f1: 0.0,
    }
    .check(&report, &transfer)
    .unwrap_err();
    assert!(error.to_string().contains("ECE upper bound"), "{error}");
}
