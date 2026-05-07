//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Log-odds conjunction wrappers (Section 4, Paper 4).

use uqa_scoring::{log_odds_conjunction, logit, prob::log_odds_conjunction_weighted, sigmoid};

/// Confidence-scaled log-odds fusion.
///
/// Preserves:
/// - n=1 identity (Proposition 4.3.2)
/// - sign preservation (Theorem 4.2.2)
/// - irrelevance / relevance preservation
/// - symmetric disagreement collapse to 0.5
///
/// Scale neutrality (`P_i = p` => `P_final = p`) holds only at `alpha = 0`;
/// the default `alpha = 0.5` deliberately amplifies agreement.
#[derive(Debug, Clone, Copy)]
pub struct LogOddsFusion {
    pub alpha: f64,
}

impl Default for LogOddsFusion {
    fn default() -> Self {
        Self { alpha: 0.5 }
    }
}

impl LogOddsFusion {
    pub fn new(alpha: f64) -> Self {
        Self { alpha }
    }

    pub fn with_gating(alpha: f64, _gating: Option<&str>) -> Self {
        Self { alpha }
    }

    pub fn fuse(&self, probs: &[f64]) -> f64 {
        match probs.len() {
            0 => 0.5,
            1 => probs[0],
            _ => log_odds_conjunction(probs, self.alpha),
        }
    }

    /// Scale-neutral mean log-odds (Definition 4.1.1, Paper 4): no
    /// confidence amplification — `alpha = 0` regardless of `self.alpha`.
    pub fn fuse_mean(&self, probs: &[f64]) -> f64 {
        match probs.len() {
            0 => 0.5,
            1 => probs[0],
            _ => {
                let n = probs.len() as f64;
                let mean: f64 = probs.iter().map(|&p| logit(p)).sum::<f64>() / n;
                sigmoid(mean)
            }
        }
    }

    pub fn fuse_weighted(&self, probs: &[f64], weights: &[f64]) -> Result<f64, &'static str> {
        if probs.is_empty() {
            return Ok(0.5);
        }
        log_odds_conjunction_weighted(probs, weights, self.alpha)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SignalQuality {
    pub coverage_ratio: f64,
    pub score_variance: f64,
    pub calibration_error: f64,
}

/// Per-signal adaptive confidence: better coverage / lower variance /
/// lower calibration error -> higher weight.
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

    pub fn signal_alpha(&self, q: SignalQuality) -> f64 {
        let coverage = q.coverage_ratio.clamp(0.0, 1.0);
        let cal_error = q.calibration_error.clamp(0.0, 1.0);
        let variance = q.score_variance.max(0.0);
        let alpha = self.base_alpha * (coverage * (1.0 - cal_error)) / (1.0 + variance);
        alpha.clamp(0.01, 1.0)
    }

    pub fn compute_signal_alpha(&self, q: SignalQuality) -> f64 {
        self.signal_alpha(q)
    }

    pub fn fuse(&self, probs: &[f64], qualities: &[SignalQuality]) -> Result<f64, &'static str> {
        match probs.len() {
            0 => Ok(0.5),
            1 => Ok(probs[0]),
            _ => {
                if probs.len() != qualities.len() {
                    return Err("probs and qualities must have the same length");
                }
                let raw: Vec<f64> = qualities.iter().map(|q| self.signal_alpha(*q)).collect();
                let total: f64 = raw.iter().sum();
                if total == 0.0 {
                    return Err("computed weights sum to zero");
                }
                let normalized: Vec<f64> = raw.iter().map(|w| w / total).collect();
                let inner = LogOddsFusion::new(self.base_alpha);
                inner.fuse_weighted(probs, &normalized)
            }
        }
    }

    pub fn fuse_adaptive(
        &self,
        probs: &[f64],
        qualities: &[SignalQuality],
    ) -> Result<f64, &'static str> {
        self.fuse(probs, qualities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {a} ~ {b}");
    }

    #[test]
    fn fuse_n1_identity() {
        let f = LogOddsFusion::default();
        approx_eq(f.fuse(&[0.7]), 0.7);
    }

    #[test]
    fn fuse_mean_scale_neutral() {
        let f = LogOddsFusion::default();
        for p in [0.2, 0.5, 0.8] {
            let probs = [p; 4];
            approx_eq(f.fuse_mean(&probs), p);
        }
    }

    #[test]
    fn adaptive_signal_alpha_clamped() {
        let f = AdaptiveLogOddsFusion::default();
        let q = SignalQuality {
            coverage_ratio: 0.0,
            score_variance: 100.0,
            calibration_error: 1.0,
        };
        let a = f.signal_alpha(q);
        assert!((0.01..=1.0).contains(&a));
    }
}
