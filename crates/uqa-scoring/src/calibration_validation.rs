//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Held-out calibration validation with deterministic uncertainty estimates.
//!
//! These utilities do not turn an unlabeled transform into a probability
//! model. They evaluate already-produced probabilities on a held-out target
//! population, attach bootstrap confidence intervals, and transfer a decision
//! threshold selected on a disjoint validation split without retuning it.

use crate::error::{invalid_input, require_finite, require_probability};
use crate::{CalibrationMetrics, CalibrationReport, ScoringResult};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BootstrapConfig {
    pub resamples: usize,
    pub confidence_level: f64,
    pub seed: u64,
}

impl BootstrapConfig {
    pub fn validate(self) -> ScoringResult<()> {
        if self.resamples < 2 {
            return Err(invalid_input(format!(
                "bootstrap resamples must be at least 2, got {}",
                self.resamples
            )));
        }
        require_finite(self.confidence_level, "bootstrap confidence_level")?;
        if self.confidence_level <= 0.0 || self.confidence_level >= 1.0 {
            return Err(invalid_input(format!(
                "bootstrap confidence_level must be in (0, 1), got {}",
                self.confidence_level
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfidenceInterval {
    pub lower: f64,
    pub upper: f64,
    pub confidence_level: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeldOutCalibrationReport {
    pub point: CalibrationReport,
    pub ece_interval: ConfidenceInterval,
    pub brier_interval: ConfidenceInterval,
    pub log_loss_interval: ConfidenceInterval,
    pub sample_count: usize,
    pub bootstrap: BootstrapConfig,
}

impl HeldOutCalibrationReport {
    pub fn evaluate(
        probabilities: &[f64],
        labels: &[u8],
        n_bins: usize,
        bootstrap: BootstrapConfig,
    ) -> ScoringResult<Self> {
        bootstrap.validate()?;
        if probabilities.is_empty() {
            return Err(invalid_input(
                "held-out calibration evaluation requires at least one sample",
            ));
        }
        let point = CalibrationMetrics::report(probabilities, labels, n_bins)?;
        let mut rng = SplitMix64::new(bootstrap.seed);
        let mut sampled_probabilities = vec![0.0; probabilities.len()];
        let mut sampled_labels = vec![0_u8; labels.len()];
        let mut ece = Vec::with_capacity(bootstrap.resamples);
        let mut brier = Vec::with_capacity(bootstrap.resamples);
        let mut log_loss = Vec::with_capacity(bootstrap.resamples);
        for _ in 0..bootstrap.resamples {
            for index in 0..probabilities.len() {
                let sampled = rng.index(probabilities.len())?;
                sampled_probabilities[index] = probabilities[sampled];
                sampled_labels[index] = labels[sampled];
            }
            let report =
                CalibrationMetrics::report(&sampled_probabilities, &sampled_labels, n_bins)?;
            ece.push(report.ece);
            brier.push(report.brier);
            log_loss.push(report.log_loss);
        }

        Ok(Self {
            ece_interval: percentile_interval(&mut ece, point.ece, bootstrap.confidence_level),
            brier_interval: percentile_interval(
                &mut brier,
                point.brier,
                bootstrap.confidence_level,
            ),
            log_loss_interval: percentile_interval(
                &mut log_loss,
                point.log_loss,
                bootstrap.confidence_level,
            ),
            sample_count: probabilities.len(),
            point,
            bootstrap,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BinaryDecisionMetrics {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub predicted_positive: usize,
    pub actual_positive: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThresholdTransferReport {
    pub threshold: f64,
    pub validation: BinaryDecisionMetrics,
    pub held_out: BinaryDecisionMetrics,
}

impl ThresholdTransferReport {
    /// Select the F1-maximizing threshold on `validation_*`, then apply that
    /// exact threshold to the disjoint held-out split.
    pub fn evaluate(
        validation_probabilities: &[f64],
        validation_labels: &[u8],
        held_out_probabilities: &[f64],
        held_out_labels: &[u8],
    ) -> ScoringResult<Self> {
        validate_labeled_probabilities(validation_probabilities, validation_labels, "validation")?;
        validate_labeled_probabilities(held_out_probabilities, held_out_labels, "held_out")?;
        if validation_probabilities.is_empty() || held_out_probabilities.is_empty() {
            return Err(invalid_input(
                "threshold transfer requires non-empty validation and held-out splits",
            ));
        }

        let mut thresholds = validation_probabilities.to_vec();
        thresholds.push(0.0);
        thresholds.push(1.0);
        thresholds.sort_by(f64::total_cmp);
        thresholds.dedup_by(|left, right| left.to_bits() == right.to_bits());

        let mut best_threshold = thresholds[0];
        let mut best =
            decision_metrics(validation_probabilities, validation_labels, best_threshold);
        for threshold in thresholds.into_iter().skip(1) {
            let metrics = decision_metrics(validation_probabilities, validation_labels, threshold);
            let order = metrics
                .f1
                .total_cmp(&best.f1)
                .then_with(|| metrics.precision.total_cmp(&best.precision))
                .then_with(|| threshold.total_cmp(&best_threshold));
            if order.is_gt() {
                best_threshold = threshold;
                best = metrics;
            }
        }

        Ok(Self {
            threshold: best_threshold,
            validation: best,
            held_out: decision_metrics(held_out_probabilities, held_out_labels, best_threshold),
        })
    }
}

/// Regression gate applied to held-out metrics. Calibration thresholds use
/// the upper bootstrap bound, so passing means the configured maximum remains
/// satisfied at the requested confidence level. The transferred decision
/// threshold is gated on held-out F1 without selecting it on held-out labels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeldOutCalibrationGate {
    pub max_ece_upper: f64,
    pub max_brier_upper: f64,
    pub max_log_loss_upper: f64,
    pub min_transferred_f1: f64,
}

impl HeldOutCalibrationGate {
    pub fn check(
        self,
        calibration: &HeldOutCalibrationReport,
        threshold_transfer: &ThresholdTransferReport,
    ) -> ScoringResult<()> {
        require_probability(self.max_ece_upper, "max_ece_upper")?;
        require_probability(self.max_brier_upper, "max_brier_upper")?;
        require_finite(self.max_log_loss_upper, "max_log_loss_upper")?;
        if self.max_log_loss_upper < 0.0 {
            return Err(invalid_input("max_log_loss_upper must be non-negative"));
        }
        require_probability(self.min_transferred_f1, "min_transferred_f1")?;

        let mut failures = Vec::new();
        if calibration.ece_interval.upper > self.max_ece_upper {
            failures.push(format!(
                "ECE upper bound {} exceeds {}",
                calibration.ece_interval.upper, self.max_ece_upper
            ));
        }
        if calibration.brier_interval.upper > self.max_brier_upper {
            failures.push(format!(
                "Brier upper bound {} exceeds {}",
                calibration.brier_interval.upper, self.max_brier_upper
            ));
        }
        if calibration.log_loss_interval.upper > self.max_log_loss_upper {
            failures.push(format!(
                "log-loss upper bound {} exceeds {}",
                calibration.log_loss_interval.upper, self.max_log_loss_upper
            ));
        }
        if threshold_transfer.held_out.f1 < self.min_transferred_f1 {
            failures.push(format!(
                "transferred held-out F1 {} is below {}",
                threshold_transfer.held_out.f1, self.min_transferred_f1
            ));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(invalid_input(format!(
                "held-out calibration gate failed: {}",
                failures.join("; ")
            )))
        }
    }
}

fn validate_labeled_probabilities(
    probabilities: &[f64],
    labels: &[u8],
    split: &str,
) -> ScoringResult<()> {
    if probabilities.len() != labels.len() {
        return Err(invalid_input(format!(
            "{split} probabilities length {} does not match labels length {}",
            probabilities.len(),
            labels.len()
        )));
    }
    for (index, probability) in probabilities.iter().copied().enumerate() {
        require_probability(probability, &format!("{split}_probabilities[{index}]"))?;
    }
    for (index, label) in labels.iter().copied().enumerate() {
        if label > 1 {
            return Err(invalid_input(format!(
                "{split}_labels[{index}] must be 0 or 1, got {label}"
            )));
        }
    }
    Ok(())
}

fn decision_metrics(probabilities: &[f64], labels: &[u8], threshold: f64) -> BinaryDecisionMetrics {
    let mut true_positive = 0_usize;
    let mut predicted_positive = 0_usize;
    let mut actual_positive = 0_usize;
    for (&probability, &label) in probabilities.iter().zip(labels) {
        let predicted = probability >= threshold;
        if predicted {
            predicted_positive += 1;
        }
        if label == 1 {
            actual_positive += 1;
            if predicted {
                true_positive += 1;
            }
        }
    }
    let precision = ratio(true_positive, predicted_positive);
    let recall = ratio(true_positive, actual_positive);
    let f1 = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };
    BinaryDecisionMetrics {
        precision,
        recall,
        f1,
        predicted_positive,
        actual_positive,
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn percentile_interval(
    samples: &mut [f64],
    point: f64,
    confidence_level: f64,
) -> ConfidenceInterval {
    samples.sort_by(f64::total_cmp);
    let tail = (1.0 - confidence_level) / 2.0;
    let last = samples.len() - 1;
    let lower_index = (tail * last as f64).floor() as usize;
    let upper_index = ((1.0 - tail) * last as f64).ceil() as usize;
    ConfidenceInterval {
        lower: samples[lower_index.min(last)].min(point),
        upper: samples[upper_index.min(last)].max(point),
        confidence_level,
    }
}

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn index(&mut self, upper: usize) -> ScoringResult<usize> {
        let upper = u64::try_from(upper)
            .map_err(|_| invalid_input("bootstrap sample count exceeds u64 range"))?;
        usize::try_from(self.next() % upper)
            .map_err(|_| invalid_input("bootstrap index exceeds usize range"))
    }
}
