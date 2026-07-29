//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sparse log-odds fusion under the probability contract.
//!
//! Signals are prior-free evidence probabilities in `(0, 1)`; the fusion
//! prior (`base_rate`) enters exactly once, after confidence scaling:
//!
//! `P = sigmoid(aggregate(gate(logit(p_i))) * n^alpha + logit(base_rate))`
//!
//! The default `Softplus` gating is the smooth evidence floor of
//! Remark 6.5.4 (Paper 4): a matching signal never counts against a
//! document beyond the prior, while ordering among weak matches is
//! preserved. Because the gate is applied to prior-free evidence and
//! the prior enters once, the floor sits at the corpus prior -- not at
//! an unconditional p = 0.5. `Pass` gating keeps the raw sign of
//! evidence (Theorem 4.2.2) for callers that want strictly signed
//! fusion; it matches Lucene's `LogOddsFusionQuery` default since
//! Lucene PR 16410 flipped that query to signed log-odds with softplus
//! as the explicit opt-in (`Gating.SOFTPLUS`). The softplus default
//! here is deliberate and BEIR-validated: with prior-free evidence the
//! floor is the prior, which Lucene's posterior-fed softplus lacked.

use uqa_scoring::{logit as prior_logit, sigmoid};

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

    /// Apply this gate to a logit-domain value.
    pub fn apply(self, value: f64) -> f64 {
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
/// Matching signals contribute `softplus(logit(p))` by default -- the
/// smooth evidence floor; a signal that did not match contributes
/// exactly zero, while the denominator and confidence scale still use
/// the total signal count. A configured `base_rate` adds
/// `logit(base_rate)` exactly once after confidence scaling, so
/// signals must be prior-free evidence.
#[derive(Debug, Clone, Copy)]
pub struct LogOddsFusion {
    pub alpha: f64,
    pub gating: LogitGating,
    pub base_rate: Option<f64>,
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
            base_rate: None,
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

    /// Fusion-level relevance prior, applied exactly once. Signals fed
    /// into a prior-configured fusion must be prior-free evidence.
    pub fn with_base_rate(mut self, base_rate: f64) -> Self {
        assert!(
            base_rate.is_finite() && base_rate > 0.0 && base_rate < 1.0,
            "base_rate must be in (0, 1), got {base_rate}"
        );
        self.base_rate = Some(base_rate);
        self
    }

    fn logit_base_rate(&self) -> f64 {
        self.base_rate.map_or(0.0, prior_logit)
    }

    /// The gated evidence logit a probability would contribute to this
    /// fusion. Exposed so callers can derive per-signal statistics
    /// (e.g. discrimination-based weights) from the same transform the
    /// fusion applies.
    pub fn gated_logit(&self, probability: f64) -> f64 {
        self.gating.apply(lucene_logit(probability))
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
            return Ok(self.base_rate.unwrap_or(0.5));
        }

        // The single-signal identity (Proposition 4.3.2) only holds for
        // prior-free fusion without normalization bounds; a configured
        // prior must still enter once, and explicit logit bounds are a
        // learned per-signal transform that must not be bypassed.
        if probabilities.len() == 1 && self.base_rate.is_none() && logit_min.is_none() {
            return Ok(probabilities[0].unwrap_or(0.5));
        }

        let mut logit_sum = 0.0;
        for (index, probability) in probabilities.iter().enumerate() {
            let Some(probability) = probability else {
                continue;
            };
            let raw_logit = lucene_logit(*probability);
            let gated = match (logit_min, logit_max) {
                // Validation guarantees min < max, so the range is
                // strictly positive here.
                (Some(minimums), Some(maximums)) => ((raw_logit - minimums[index])
                    / (maximums[index] - minimums[index]))
                    .clamp(0.0, 1.0),
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
        Ok(sigmoid(
            aggregate * signal_count.powf(self.alpha) + self.logit_base_rate(),
        ))
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
    /// A configured `base_rate` still enters exactly once.
    pub fn fuse_mean(&self, probabilities: &[f64]) -> f64 {
        match (probabilities.len(), self.base_rate) {
            (0, prior) => prior.unwrap_or(0.5),
            (1, None) => probabilities[0],
            _ => sigmoid(
                probabilities
                    .iter()
                    .map(|probability| lucene_logit(*probability))
                    .sum::<f64>()
                    / probabilities.len() as f64
                    + self.logit_base_rate(),
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
    let (minimums, maximums) = match (logit_min, logit_max) {
        (None, None) => return Ok(()),
        (Some(minimums), Some(maximums)) => (minimums, maximums),
        _ => return Err("logit_min and logit_max must either both be set or absent"),
    };
    if minimums.len() != signal_count || maximums.len() != signal_count {
        return Err("logit bound lengths must equal signal count");
    }
    if minimums.iter().zip(maximums).any(|(minimum, maximum)| {
        !minimum.is_finite() || !maximum.is_finite() || minimum >= maximum
    }) {
        return Err("logit bounds must be finite with min < max");
    }
    Ok(())
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
    pub gating: LogitGating,
}

impl Default for AdaptiveLogOddsFusion {
    fn default() -> Self {
        Self::new(0.5)
    }
}

impl AdaptiveLogOddsFusion {
    pub fn new(base_alpha: f64) -> Self {
        Self {
            base_alpha,
            gating: LogitGating::Softplus,
        }
    }

    pub fn with_gating(mut self, gating: LogitGating) -> Self {
        self.gating = gating;
        self
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
                let mut inner = LogOddsFusion::new(self.base_alpha);
                inner.gating = self.gating;
                inner.fuse_weighted(probabilities, &normalized)
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
    fn default_matches_lucene_formula_without_a_prior() {
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
    fn pass_gating_preserves_evidence_sign() {
        let fusion = LogOddsFusion::with_gating(0.5, Some("pass"));
        let probabilities = [0.8, 0.6];
        let logit_sum = probabilities
            .iter()
            .map(|probability| lucene_logit(*probability))
            .sum::<f64>();
        let expected = sigmoid((logit_sum / 2.0) * 2.0_f64.sqrt());
        approx_eq(fusion.fuse(&probabilities), expected);
        assert!(fusion.fuse(&[0.2, 0.2]) < 0.5, "weak evidence must sink");
    }

    #[test]
    fn absent_signal_contributes_zero_but_remains_in_denominator() {
        let fusion = LogOddsFusion::new(0.5);
        let expected = sigmoid((softplus(lucene_logit(0.8)) / 2.0) * 2.0_f64.sqrt());
        approx_eq(fusion.fuse_sparse(&[Some(0.8), None]), expected);
    }

    #[test]
    fn matches_floor_at_the_prior_rather_than_sinking() {
        // Softplus floors match evidence at zero, so with a configured
        // prior even the weakest match cannot fall below the prior;
        // pass gating lets weak evidence sink beneath it.
        let fusion = LogOddsFusion::new(0.5).with_base_rate(0.05);
        approx_eq(fusion.fuse_sparse(&[None, None]), 0.05);
        assert!(fusion.fuse_sparse(&[Some(0.1), None]) >= 0.05);
        let signed = LogOddsFusion::with_gating(0.5, Some("pass")).with_base_rate(0.05);
        assert!(signed.fuse_sparse(&[Some(0.1), None]) < 0.05);
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
    fn base_rate_enters_once_after_confidence_scaling() {
        let fusion = LogOddsFusion::new(0.5).with_base_rate(0.05);
        let probabilities = [0.8, 0.6];
        let gated_sum = softplus(lucene_logit(0.8)) + softplus(lucene_logit(0.6));
        let prior = (0.05_f64 / 0.95).ln();
        let expected = sigmoid((gated_sum / 2.0) * 2.0_f64.sqrt() + prior);
        approx_eq(fusion.fuse(&probabilities), expected);
    }

    #[test]
    fn base_rate_applies_even_to_a_single_signal() {
        let fusion = LogOddsFusion::new(0.5).with_base_rate(0.05);
        let prior = (0.05_f64 / 0.95).ln();
        approx_eq(
            fusion.fuse(&[0.8]),
            sigmoid(softplus(lucene_logit(0.8)) + prior),
        );
        approx_eq(fusion.fuse(&[]), 0.05);
        approx_eq(fusion.fuse_sparse(&[None, None]), 0.05);
    }

    #[test]
    fn neutral_evidence_returns_the_prior_under_pass_gating() {
        let fusion = LogOddsFusion::with_gating(0.5, Some("pass")).with_base_rate(0.1);
        approx_eq(fusion.fuse(&[0.5, 0.5]), 0.1);
    }

    #[test]
    fn logit_normalization_replaces_gating() {
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
    fn partial_logit_bounds_are_rejected() {
        let fusion = LogOddsFusion::new(0.5);
        let probabilities = [Some(0.8), Some(0.6)];
        let minimums = [-1.0, 0.0];
        assert_eq!(
            fusion.fuse_configured(&probabilities, None, Some(&minimums), None),
            Err("logit_min and logit_max must either both be set or absent"),
        );
        assert_eq!(
            fusion.fuse_configured(&probabilities, None, None, Some(&minimums)),
            Err("logit_min and logit_max must either both be set or absent"),
        );
    }

    #[test]
    fn degenerate_logit_bounds_are_rejected() {
        let fusion = LogOddsFusion::new(0.5);
        let probabilities = [Some(0.8), Some(0.6)];
        let inverted = ([0.0, 1.0], [1.0, 1.0]);
        assert_eq!(
            fusion.fuse_configured(&probabilities, None, Some(&inverted.0), Some(&inverted.1)),
            Err("logit bounds must be finite with min < max"),
        );
        let non_finite = ([f64::NEG_INFINITY, 0.0], [1.0, 1.0]);
        assert_eq!(
            fusion.fuse_configured(
                &probabilities,
                None,
                Some(&non_finite.0),
                Some(&non_finite.1)
            ),
            Err("logit bounds must be finite with min < max"),
        );
    }

    #[test]
    fn single_signal_with_bounds_applies_normalization() {
        // Explicit logit bounds are a learned per-signal transform, so
        // the single-signal identity must not bypass them.
        let fusion = LogOddsFusion::new(0.5);
        let minimums = [0.0];
        let maximums = [2.0 * lucene_logit(0.8)];
        let expected = sigmoid(0.5);
        approx_eq(
            fusion
                .fuse_configured(&[Some(0.8)], None, Some(&minimums), Some(&maximums))
                .unwrap(),
            expected,
        );
    }

    #[test]
    fn adaptive_fusion_honors_gating() {
        let quality = SignalQuality {
            coverage_ratio: 1.0,
            score_variance: 0.0,
            calibration_error: 0.0,
        };
        let probabilities = [0.2, 0.3];
        let softplus_fused = AdaptiveLogOddsFusion::new(0.5)
            .fuse(&probabilities, &[quality, quality])
            .unwrap();
        let pass_fused = AdaptiveLogOddsFusion::new(0.5)
            .with_gating(LogitGating::Pass)
            .fuse(&probabilities, &[quality, quality])
            .unwrap();
        assert!(
            softplus_fused > 0.5,
            "softplus floors weak evidence, got {softplus_fused}"
        );
        assert!(
            pass_fused < 0.5,
            "pass gating lets weak evidence sink, got {pass_fused}"
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
