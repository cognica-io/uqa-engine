//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sparse log-odds fusion with Lucene-compatible softplus gating.

use uqa_scoring::sigmoid;

const CLAMP_MIN: f64 = 1e-7;
const CLAMP_MAX: f64 = 1.0 - CLAMP_MIN;
const WEIGHT_SUM_TOLERANCE: f64 = 1e-3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogitGating {
    Softplus,
    Pass,
    Sigmoid,
    ReLU,
    Swish,
    Gelu,
}

impl LogitGating {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "softplus" => Some(Self::Softplus),
            "pass" | "none" => Some(Self::Pass),
            "sigmoid" => Some(Self::Sigmoid),
            "relu" => Some(Self::ReLU),
            "swish" => Some(Self::Swish),
            "gelu" => Some(Self::Gelu),
            _ => None,
        }
    }

    fn apply(self, value: f64) -> f64 {
        match self {
            Self::Softplus => softplus(value),
            Self::Pass => value,
            Self::Sigmoid => sigmoid(value),
            Self::ReLU => value.max(0.0),
            Self::Swish => value * sigmoid(value),
            Self::Gelu => {
                let inner =
                    (2.0 / std::f64::consts::PI).sqrt() * (value + 0.044_715 * value.powi(3));
                0.5 * value * (1.0 + inner.tanh())
            }
        }
    }
}

/// Confidence-scaled sparse log-odds fusion.
///
/// Matching signals contribute `softplus(logit(p))` by default. A signal
/// that did not match contributes exactly zero, while the denominator and
/// confidence scale still use the total signal count.
#[derive(Debug, Clone, Copy)]
pub struct LogOddsFusion {
    pub alpha: f64,
    pub gating: LogitGating,
}

impl Default for LogOddsFusion {
    fn default() -> Self {
        Self::new(0.5)
    }
}

impl LogOddsFusion {
    pub fn new(alpha: f64) -> Self {
        assert!(
            alpha.is_finite() && (0.0..=1.0).contains(&alpha),
            "alpha must be in [0, 1], got {alpha}"
        );
        Self {
            alpha,
            gating: LogitGating::Softplus,
        }
    }

    pub fn with_gating(alpha: f64, gating: Option<&str>) -> Self {
        let mut fusion = Self::new(alpha);
        if let Some(name) = gating {
            fusion.gating = LogitGating::parse(name)
                .unwrap_or_else(|| panic!("unknown logit gating function: {name}"));
        }
        fusion
    }

    pub fn fuse(&self, probabilities: &[f64]) -> f64 {
        let sparse: Vec<Option<f64>> = probabilities.iter().copied().map(Some).collect();
        self.fuse_sparse(&sparse)
    }

    pub fn fuse_sparse(&self, probabilities: &[Option<f64>]) -> f64 {
        self.fuse_configured(probabilities, None, None, None)
            .expect("uniform fusion configuration is valid")
    }

    pub fn fuse_weighted(
        &self,
        probabilities: &[f64],
        weights: &[f64],
    ) -> Result<f64, &'static str> {
        let sparse: Vec<Option<f64>> = probabilities.iter().copied().map(Some).collect();
        self.fuse_weighted_sparse(&sparse, weights)
    }

    pub fn fuse_weighted_sparse(
        &self,
        probabilities: &[Option<f64>],
        weights: &[f64],
    ) -> Result<f64, &'static str> {
        self.fuse_configured(probabilities, Some(weights), None, None)
    }

    pub fn fuse_configured(
        &self,
        probabilities: &[Option<f64>],
        weights: Option<&[f64]>,
        logit_min: Option<&[f64]>,
        logit_max: Option<&[f64]>,
    ) -> Result<f64, &'static str> {
        self.validate_configuration(probabilities.len(), weights, logit_min, logit_max)?;
        if probabilities.is_empty() {
            return Ok(0.5);
        }

        if probabilities.len() == 1 {
            return Ok(probabilities[0].unwrap_or(0.5));
        }

        let mut logit_sum = 0.0;
        for (index, probability) in probabilities.iter().enumerate() {
            let Some(probability) = probability else {
                continue;
            };
            let raw_logit = lucene_logit(*probability);
            let gated = match (logit_min, logit_max) {
                (Some(minimums), Some(maximums)) => {
                    let range = maximums[index] - minimums[index];
                    if range > 0.0 {
                        ((raw_logit - minimums[index]) / range).clamp(0.0, 1.0)
                    } else {
                        0.5
                    }
                }
                _ => self.gating.apply(raw_logit),
            };
            logit_sum += weights.map_or(gated, |signal_weights| signal_weights[index] * gated);
        }

        let signal_count = probabilities.len() as f64;
        let aggregate = if weights.is_some() {
            logit_sum
        } else {
            logit_sum / signal_count
        };
        Ok(sigmoid(aggregate * signal_count.powf(self.alpha)))
    }

    pub fn validate_configuration(
        &self,
        signal_count: usize,
        weights: Option<&[f64]>,
        logit_min: Option<&[f64]>,
        logit_max: Option<&[f64]>,
    ) -> Result<(), &'static str> {
        validate_weights(signal_count, weights)?;
        validate_bounds(signal_count, logit_min, logit_max)
    }

    /// Raw mean-logit aggregation without gating or confidence scaling.
    pub fn fuse_mean(&self, probabilities: &[f64]) -> f64 {
        match probabilities.len() {
            0 => 0.5,
            1 => probabilities[0],
            _ => sigmoid(
                probabilities
                    .iter()
                    .map(|probability| lucene_logit(*probability))
                    .sum::<f64>()
                    / probabilities.len() as f64,
            ),
        }
    }
}

fn validate_weights(signal_count: usize, weights: Option<&[f64]>) -> Result<(), &'static str> {
    let Some(weights) = weights else {
        return Ok(());
    };
    if weights.len() != signal_count {
        return Err("weights length must equal signal count");
    }
    if weights
        .iter()
        .any(|weight| !weight.is_finite() || *weight < 0.0)
    {
        return Err("weights must be non-negative and finite");
    }
    if (weights.iter().sum::<f64>() - 1.0).abs() > WEIGHT_SUM_TOLERANCE {
        return Err("weights must sum to 1");
    }
    Ok(())
}

fn validate_bounds(
    signal_count: usize,
    logit_min: Option<&[f64]>,
    logit_max: Option<&[f64]>,
) -> Result<(), &'static str> {
    match (logit_min, logit_max) {
        (Some(minimums), Some(maximums))
            if minimums.len() == signal_count && maximums.len() == signal_count =>
        {
            Ok(())
        }
        (Some(_), Some(_)) => Err("logit bound lengths must equal signal count"),
        _ => Ok(()),
    }
}

fn lucene_logit(probability: f64) -> f64 {
    let clamped = probability.clamp(CLAMP_MIN, CLAMP_MAX);
    (clamped / (1.0 - clamped)).ln()
}

fn softplus(value: f64) -> f64 {
    if value > 20.0 {
        value
    } else {
        value.exp().ln_1p()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SignalQuality {
    pub coverage_ratio: f64,
    pub score_variance: f64,
    pub calibration_error: f64,
}

/// Per-signal adaptive confidence: better coverage, lower variance, and
/// lower calibration error produce a higher normalized weight.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveLogOddsFusion {
    pub base_alpha: f64,
}

impl Default for AdaptiveLogOddsFusion {
    fn default() -> Self {
        Self { base_alpha: 0.5 }
    }
}

impl AdaptiveLogOddsFusion {
    pub fn new(base_alpha: f64) -> Self {
        Self { base_alpha }
    }

    pub fn signal_alpha(&self, quality: SignalQuality) -> f64 {
        let coverage = quality.coverage_ratio.clamp(0.0, 1.0);
        let calibration_error = quality.calibration_error.clamp(0.0, 1.0);
        let variance = quality.score_variance.max(0.0);
        let alpha = self.base_alpha * (coverage * (1.0 - calibration_error)) / (1.0 + variance);
        alpha.clamp(0.01, 1.0)
    }

    pub fn compute_signal_alpha(&self, quality: SignalQuality) -> f64 {
        self.signal_alpha(quality)
    }

    pub fn fuse(
        &self,
        probabilities: &[f64],
        qualities: &[SignalQuality],
    ) -> Result<f64, &'static str> {
        match probabilities.len() {
            0 => Ok(0.5),
            1 => Ok(probabilities[0]),
            _ => {
                if probabilities.len() != qualities.len() {
                    return Err("probabilities and qualities must have the same length");
                }
                let raw: Vec<f64> = qualities
                    .iter()
                    .map(|quality| self.signal_alpha(*quality))
                    .collect();
                let total: f64 = raw.iter().sum();
                if total == 0.0 {
                    return Err("computed weights sum to zero");
                }
                let normalized: Vec<f64> = raw.iter().map(|weight| weight / total).collect();
                LogOddsFusion::new(self.base_alpha).fuse_weighted(probabilities, &normalized)
            }
        }
    }

    pub fn fuse_adaptive(
        &self,
        probabilities: &[f64],
        qualities: &[SignalQuality],
    ) -> Result<f64, &'static str> {
        self.fuse(probabilities, qualities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-12, "expected {left} ~= {right}");
    }

    #[test]
    fn single_signal_rewrites_to_identity() {
        approx_eq(LogOddsFusion::default().fuse(&[0.7]), 0.7);
    }

    #[test]
    fn uniform_softplus_matches_lucene_formula() {
        let fusion = LogOddsFusion::new(0.5);
        let probabilities = [0.8, 0.6];
        let gated_sum = probabilities
            .iter()
            .map(|probability| softplus(lucene_logit(*probability)))
            .sum::<f64>();
        let expected = sigmoid((gated_sum / 2.0) * 2.0_f64.sqrt());
        approx_eq(fusion.fuse(&probabilities), expected);
    }

    #[test]
    fn absent_signal_contributes_zero_but_remains_in_denominator() {
        let fusion = LogOddsFusion::new(0.5);
        let expected = sigmoid((softplus(lucene_logit(0.8)) / 2.0) * 2.0_f64.sqrt());
        approx_eq(fusion.fuse_sparse(&[Some(0.8), None]), expected);
    }

    #[test]
    fn weak_match_scores_above_complete_absence() {
        let fusion = LogOddsFusion::new(0.5);
        assert!(fusion.fuse_sparse(&[Some(0.1), None]) > 0.5);
        approx_eq(fusion.fuse_sparse(&[None, None]), 0.5);
    }

    #[test]
    fn weighted_fusion_uses_weighted_sum_without_mean_division() {
        let fusion = LogOddsFusion::new(0.5);
        let probabilities = [Some(0.8), Some(0.6)];
        let weights = [0.75, 0.25];
        let expected = sigmoid(
            (0.75 * softplus(lucene_logit(0.8)) + 0.25 * softplus(lucene_logit(0.6)))
                * 2.0_f64.sqrt(),
        );
        approx_eq(
            fusion
                .fuse_weighted_sparse(&probabilities, &weights)
                .unwrap(),
            expected,
        );
    }

    #[test]
    fn logit_normalization_replaces_softplus() {
        let fusion = LogOddsFusion::new(0.0);
        let probabilities = [Some(0.5), Some(0.8)];
        let minimums = [-1.0, 0.0];
        let maximums = [1.0, lucene_logit(0.8)];
        let expected = sigmoid(f64::midpoint(0.5, 1.0));
        approx_eq(
            fusion
                .fuse_configured(&probabilities, None, Some(&minimums), Some(&maximums))
                .unwrap(),
            expected,
        );
    }

    #[test]
    fn partial_logit_bounds_fall_back_to_softplus() {
        let fusion = LogOddsFusion::new(0.5);
        let probabilities = [Some(0.8), Some(0.6)];
        let minimums = [-1.0, 0.0];
        approx_eq(
            fusion
                .fuse_configured(&probabilities, None, Some(&minimums), None)
                .unwrap(),
            fusion.fuse_sparse(&probabilities),
        );
    }

    #[test]
    fn adaptive_signal_alpha_is_clamped() {
        let fusion = AdaptiveLogOddsFusion::default();
        let quality = SignalQuality {
            coverage_ratio: 0.0,
            score_variance: 100.0,
            calibration_error: 1.0,
        };
        assert!((0.01..=1.0).contains(&fusion.signal_alpha(quality)));
    }
}
