//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Calibration diagnostics (Paper 3 Section 11.3, Paper 5 Section 8.3).

use crate::error::{invalid_input, require_finite, require_probability};
use crate::prob::PROB_EPSILON;
use crate::{ScoringError, ScoringResult};

const MAX_EXACT_F64_INTEGER: u64 = 1u64 << f64::MANTISSA_DIGITS;

/// Likelihood-ratio calibrator for vector distances (Theorem 3.1.1,
/// Paper 5). Converts vector similarity into calibrated probability.
///
/// The transform models distances to relevant documents (`f_R`) and
/// to background (random) documents (`f_G`) as Gaussian distributions.
/// Calibration converts a distance into a posterior probability by
/// applying Bayes' rule with the configured base rate:
///
/// ```text
/// log f_R(d) - log f_G(d) + logit(base_rate)  ->  sigmoid
/// ```
///
/// The formulation is deliberately small; downstream callers fit the
/// means and standard deviations offline (for example, via the parameter
/// learner) and pass the transform through. Optional per-distance
/// weights bias the computed log-odds before the sigmoid.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VectorProbabilityTransform {
    /// Mean distance for relevant documents (numerator distribution).
    pub mu_match: f64,
    /// Mean distance for background documents (denominator distribution).
    pub mu_random: f64,
    /// Shared standard deviation. Must be positive.
    pub sigma: f64,
    /// Prior probability of relevance. Default 0.5 (neutral).
    pub base_rate: f64,
}

impl VectorProbabilityTransform {
    pub fn new(mu_match: f64, mu_random: f64, sigma: f64, base_rate: f64) -> ScoringResult<Self> {
        require_finite(mu_match, "mu_match")?;
        require_finite(mu_random, "mu_random")?;
        require_finite(sigma, "sigma")?;
        if sigma <= 0.0 {
            return Err(invalid_input(format!(
                "sigma must be positive, got {sigma}"
            )));
        }
        require_probability(base_rate, "base_rate")?;
        if base_rate == 0.0 || base_rate == 1.0 {
            return Err(invalid_input(format!(
                "base_rate must be strictly between 0 and 1, got {base_rate}"
            )));
        }
        Ok(Self {
            mu_match,
            mu_random,
            sigma,
            base_rate,
        })
    }

    /// Convert a single distance to a probability via the likelihood
    /// ratio + base-rate logit.
    pub fn calibrate_one(&self, distance: f64) -> ScoringResult<f64> {
        let log_lr = self.log_likelihood_ratio(distance)?;
        let logit_prior = (self.base_rate / (1.0 - self.base_rate)).ln();
        let logit_post = log_lr + logit_prior;
        if !logit_post.is_finite() {
            return Err(ScoringError::ArithmeticOverflow(format!(
                "calibration logit is not finite for distance {distance}"
            )));
        }
        Ok(1.0 / (1.0 + (-logit_post).exp()))
    }

    /// Vectorized calibration. Optional `weights` bias each distance's
    /// log-odds before the sigmoid.
    pub fn calibrate(&self, distances: &[f64], weights: Option<&[f64]>) -> ScoringResult<Vec<f64>> {
        if let Some(weights) = weights {
            if weights.len() != distances.len() {
                return Err(invalid_input(format!(
                    "weights length {} does not match distances length {}",
                    weights.len(),
                    distances.len()
                )));
            }
            for (index, weight) in weights.iter().copied().enumerate() {
                require_finite(weight, &format!("weights[{index}]"))?;
            }
        }

        distances
            .iter()
            .copied()
            .enumerate()
            .map(|(index, distance)| {
                let posterior = self.calibrate_one(distance)?;
                let Some(weight) = weights.map(|values| values[index]) else {
                    return Ok(posterior);
                };
                let posterior = posterior.clamp(PROB_EPSILON, 1.0 - PROB_EPSILON);
                let logit = (posterior / (1.0 - posterior)).ln() + weight;
                if !logit.is_finite() {
                    return Err(ScoringError::ArithmeticOverflow(format!(
                        "weighted calibration logit is not finite at index {index}"
                    )));
                }
                Ok(1.0 / (1.0 + (-logit).exp()))
            })
            .collect()
    }

    fn log_likelihood_ratio(&self, distance: f64) -> ScoringResult<f64> {
        require_finite(distance, "distance")?;
        // Gaussian log-LR with shared sigma:
        // (mu_R - d)^2 / (2 sigma^2) - (mu_G - d)^2 / (2 sigma^2)
        // collapses to a linear function of d.
        let twosq = 2.0 * self.sigma * self.sigma;
        let r = (self.mu_match - distance).powi(2) / twosq;
        let g = (self.mu_random - distance).powi(2) / twosq;
        // Numerator should DOMINATE for small (near-relevant) distances,
        // so the log-LR is `g - r`.
        let ratio = g - r;
        if ratio.is_finite() {
            Ok(ratio)
        } else {
            Err(ScoringError::ArithmeticOverflow(format!(
                "likelihood ratio is not finite for distance {distance}"
            )))
        }
    }
}

pub struct CalibrationMetrics;

#[derive(Debug, Clone, PartialEq)]
pub struct ReliabilityBin {
    pub avg_predicted: f64,
    pub avg_actual: f64,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationReport {
    pub ece: f64,
    pub brier: f64,
    pub log_loss: f64,
    pub bins: Vec<ReliabilityBin>,
}

impl CalibrationMetrics {
    pub fn log_loss(probabilities: &[f64], labels: &[u8]) -> ScoringResult<f64> {
        validate_metric_inputs(probabilities, labels)?;
        if probabilities.is_empty() {
            return Ok(0.0);
        }
        let n = exact_usize_as_f64(probabilities.len(), "probability count")?;
        let mut s = 0.0;
        for (&p, &y) in probabilities.iter().zip(labels) {
            let pp = p.clamp(PROB_EPSILON, 1.0 - PROB_EPSILON);
            let y = f64::from(y);
            s += y * pp.ln() + (1.0 - y) * (1.0 - pp).ln();
            if !s.is_finite() {
                return Err(ScoringError::ArithmeticOverflow(
                    "log-loss accumulation is not finite".to_string(),
                ));
            }
        }
        Ok(-s / n)
    }

    pub fn brier(probabilities: &[f64], labels: &[u8]) -> ScoringResult<f64> {
        validate_metric_inputs(probabilities, labels)?;
        if probabilities.is_empty() {
            return Ok(0.0);
        }
        let n = exact_usize_as_f64(probabilities.len(), "probability count")?;
        let mut sum = 0.0;
        for (&probability, &label) in probabilities.iter().zip(labels) {
            sum += (probability - f64::from(label)).powi(2);
            if !sum.is_finite() {
                return Err(ScoringError::ArithmeticOverflow(
                    "Brier score accumulation is not finite".to_string(),
                ));
            }
        }
        Ok(sum / n)
    }

    pub fn ece(probabilities: &[f64], labels: &[u8], n_bins: usize) -> ScoringResult<f64> {
        validate_metric_inputs(probabilities, labels)?;
        validate_bin_count(n_bins)?;
        let total = probabilities.len();
        if total == 0 {
            return Ok(0.0);
        }
        let total_f64 = exact_usize_as_f64(total, "probability count")?;
        let mut acc = 0.0;
        for bin in reliability_bins(probabilities, labels, n_bins)? {
            let bin_count = exact_usize_as_f64(bin.count, "reliability bin count")?;
            acc += (bin_count / total_f64) * (bin.avg_predicted - bin.avg_actual).abs();
        }
        Ok(acc)
    }

    pub fn report(
        probabilities: &[f64],
        labels: &[u8],
        n_bins: usize,
    ) -> ScoringResult<CalibrationReport> {
        validate_metric_inputs(probabilities, labels)?;
        validate_bin_count(n_bins)?;
        let bins = reliability_bins(probabilities, labels, n_bins)?;
        let total = exact_usize_as_f64(probabilities.len(), "probability count")?;
        let ece = if total > 0.0 {
            let mut sum = 0.0;
            for bin in &bins {
                let count = exact_usize_as_f64(bin.count, "reliability bin count")?;
                sum += (count / total) * (bin.avg_predicted - bin.avg_actual).abs();
            }
            sum
        } else {
            0.0
        };
        Ok(CalibrationReport {
            ece,
            brier: Self::brier(probabilities, labels)?,
            log_loss: Self::log_loss(probabilities, labels)?,
            bins,
        })
    }

    pub fn reliability_diagram(
        probabilities: &[f64],
        labels: &[u8],
        n_bins: usize,
    ) -> ScoringResult<Vec<ReliabilityBin>> {
        validate_metric_inputs(probabilities, labels)?;
        validate_bin_count(n_bins)?;
        reliability_bins(probabilities, labels, n_bins)
    }
}

fn reliability_bins(
    probabilities: &[f64],
    labels: &[u8],
    n_bins: usize,
) -> ScoringResult<Vec<ReliabilityBin>> {
    if probabilities.is_empty() {
        return Ok(Vec::new());
    }
    let mut bins: Vec<(f64, f64, usize)> = vec![(0.0, 0.0, 0); n_bins];
    let n_bins_f = exact_usize_as_f64(n_bins, "reliability bin count")?;
    // Bin boundaries match the upstream UQA ECE contract: lowest bin is
    // `[lo, hi]` (inclusive both ends), the rest are `(lo, hi]`. Floor
    // division places exact upper edges into the higher bin, except for
    // `p == 0.0` which belongs to bin 0.
    for (&p, &y) in probabilities.iter().zip(labels) {
        let mut idx = (p * n_bins_f) as usize;
        if idx >= n_bins {
            idx = n_bins - 1;
        }
        if p == 0.0 {
            idx = 0;
        }
        bins[idx].0 += p;
        bins[idx].1 += f64::from(y);
        bins[idx].2 = bins[idx].2.checked_add(1).ok_or_else(|| {
            ScoringError::ArithmeticOverflow("reliability bin count overflow".to_string())
        })?;
    }
    bins.into_iter()
        .map(|(sum_p, sum_y, count)| -> ScoringResult<_> {
            if count == 0 {
                Ok(ReliabilityBin {
                    avg_predicted: 0.0,
                    avg_actual: 0.0,
                    count: 0,
                })
            } else {
                let count_f64 = exact_usize_as_f64(count, "reliability bin count")?;
                Ok(ReliabilityBin {
                    avg_predicted: sum_p / count_f64,
                    avg_actual: sum_y / count_f64,
                    count,
                })
            }
        })
        .collect()
}

fn validate_metric_inputs(probabilities: &[f64], labels: &[u8]) -> ScoringResult<()> {
    if probabilities.len() != labels.len() {
        return Err(invalid_input(format!(
            "probabilities length {} does not match labels length {}",
            probabilities.len(),
            labels.len()
        )));
    }
    for (index, probability) in probabilities.iter().copied().enumerate() {
        require_probability(probability, &format!("probabilities[{index}]"))?;
    }
    for (index, label) in labels.iter().copied().enumerate() {
        if label > 1 {
            return Err(invalid_input(format!(
                "labels[{index}] must be 0 or 1, got {label}"
            )));
        }
    }
    Ok(())
}

fn validate_bin_count(n_bins: usize) -> ScoringResult<()> {
    if n_bins == 0 {
        return Err(invalid_input("n_bins must be greater than zero"));
    }
    exact_usize_as_f64(n_bins, "n_bins").map(|_| ())
}

fn exact_usize_as_f64(value: usize, name: &str) -> ScoringResult<f64> {
    if u64::try_from(value).is_ok_and(|value| value <= MAX_EXACT_F64_INTEGER) {
        Ok(value as f64)
    } else {
        Err(invalid_input(format!(
            "{name} {value} exceeds the exact f64 integer range"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_loss_zero_for_perfect_predictions() {
        let probs = vec![1.0 - PROB_EPSILON, PROB_EPSILON, 1.0 - PROB_EPSILON];
        let labels = vec![1u8, 0, 1];
        let loss = CalibrationMetrics::log_loss(&probs, &labels).unwrap();
        assert!(loss < 1e-8, "expected ~0, got {loss}");
    }

    #[test]
    fn brier_zero_for_perfect_predictions() {
        let probs = vec![1.0, 0.0, 1.0];
        let labels = vec![1u8, 0, 1];
        let brier = CalibrationMetrics::brier(&probs, &labels).unwrap();
        assert!(brier < 1e-12);
    }

    #[test]
    fn ece_zero_when_perfectly_calibrated() {
        // Each bin's avg predicted == avg actual.
        let probs = vec![0.05; 100]; // 5% predicted
        let mut labels = vec![0u8; 100];
        for label in &mut labels[..5] {
            *label = 1;
        }
        let ece = CalibrationMetrics::ece(&probs, &labels, 10).unwrap();
        assert!(ece < 1e-9, "got {ece}");
    }

    #[test]
    fn transform_and_metrics_reject_invalid_inputs() {
        assert!(VectorProbabilityTransform::new(0.0, 1.0, 0.0, 0.5).is_err());
        assert!(VectorProbabilityTransform::new(0.0, 1.0, 1.0, 1.0).is_err());
        let transform = VectorProbabilityTransform::new(0.0, 1.0, 1.0, 0.5).unwrap();
        assert!(transform.calibrate_one(f64::NAN).is_err());
        assert!(transform.calibrate(&[0.1], Some(&[])).is_err());

        assert!(CalibrationMetrics::log_loss(&[0.5], &[]).is_err());
        assert!(CalibrationMetrics::brier(&[f64::NAN], &[0]).is_err());
        assert!(CalibrationMetrics::ece(&[0.5], &[2], 10).is_err());
        assert!(CalibrationMetrics::ece(&[0.5], &[0], 0).is_err());
    }
}
