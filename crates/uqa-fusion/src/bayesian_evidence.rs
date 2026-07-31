//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact fusion of signed, prior-free Bayesian evidence.
//!
//! Under conditional independence, likelihood-ratio evidence combines by
//! addition in log-odds space and the relevance prior enters exactly once:
//!
//! `posterior = sigmoid(prior_logit + sum(evidence_logit_i))`.
//!
//! There is deliberately no gate, confidence exponent, normalized weight, or
//! adaptive query-pool heuristic in this operator. Those policies belong to
//! [`crate::RobustPositiveEvidencePool`].

use std::fmt;

use uqa_scoring::{EvidenceLogit, PosteriorProbability, PriorLogit};

#[derive(Debug, Clone, PartialEq)]
pub enum BayesianEvidenceFusionError {
    InvalidBaseRate(f64),
    NonFiniteEvidenceSum,
}

impl fmt::Display for BayesianEvidenceFusionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBaseRate(base_rate) => write!(
                formatter,
                "base_rate must be finite and strictly between 0 and 1, got {base_rate}"
            ),
            Self::NonFiniteEvidenceSum => {
                formatter.write_str("prior and evidence logits produced a non-finite sum")
            }
        }
    }
}

impl std::error::Error for BayesianEvidenceFusionError {}

/// Exact signed-evidence fusion with one explicit relevance prior.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BayesianEvidenceFusion {
    prior: PriorLogit,
}

impl Default for BayesianEvidenceFusion {
    fn default() -> Self {
        Self {
            prior: PriorLogit::neutral(),
        }
    }
}

impl BayesianEvidenceFusion {
    pub fn new(base_rate: f64) -> Result<Self, BayesianEvidenceFusionError> {
        let prior = PriorLogit::from_probability(base_rate)
            .map_err(|_| BayesianEvidenceFusionError::InvalidBaseRate(base_rate))?;
        Ok(Self { prior })
    }

    pub const fn from_prior_logit(prior: PriorLogit) -> Self {
        Self { prior }
    }

    pub const fn prior_logit(self) -> PriorLogit {
        self.prior
    }

    /// Add every signed evidence logit and apply the prior once.
    pub fn fuse(
        &self,
        evidence: &[EvidenceLogit],
    ) -> Result<PosteriorProbability, BayesianEvidenceFusionError> {
        let mut posterior_logit = self.prior.value();
        for item in evidence {
            posterior_logit += item.value();
            if !posterior_logit.is_finite() {
                return Err(BayesianEvidenceFusionError::NonFiniteEvidenceSum);
            }
        }
        PosteriorProbability::from_logit(posterior_logit)
            .map_err(|_| BayesianEvidenceFusionError::NonFiniteEvidenceSum)
    }
}

#[cfg(test)]
mod tests {
    use uqa_scoring::{logit, sigmoid};

    use super::*;

    fn approx_eq(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-12, "expected {left} ~= {right}");
    }

    #[test]
    fn neutral_evidence_leaves_every_prior_unchanged() {
        for prior in [0.01, 0.1, 0.5, 0.9, 0.99] {
            let fusion = BayesianEvidenceFusion::new(prior).unwrap();
            let posterior = fusion
                .fuse(&[EvidenceLogit::neutral(), EvidenceLogit::neutral()])
                .unwrap();
            approx_eq(posterior.value(), prior);
        }
    }

    #[test]
    fn signed_evidence_is_summed_without_gating_or_scaling() {
        let fusion = BayesianEvidenceFusion::new(0.05).unwrap();
        let evidence = [
            EvidenceLogit::new(1.25).unwrap(),
            EvidenceLogit::new(-0.4).unwrap(),
            EvidenceLogit::new(0.15).unwrap(),
        ];
        let expected = sigmoid(logit(0.05) + 1.25 - 0.4 + 0.15);
        approx_eq(fusion.fuse(&evidence).unwrap().value(), expected);
    }

    #[test]
    fn empty_evidence_returns_the_prior() {
        let fusion = BayesianEvidenceFusion::new(0.2).unwrap();
        approx_eq(fusion.fuse(&[]).unwrap().value(), 0.2);
    }

    #[test]
    fn invalid_base_rates_and_overflow_are_errors() {
        for invalid in [f64::NAN, f64::NEG_INFINITY, 0.0, 1.0] {
            assert!(BayesianEvidenceFusion::new(invalid).is_err());
        }
        let fusion = BayesianEvidenceFusion::default();
        let evidence = [
            EvidenceLogit::new(f64::MAX).unwrap(),
            EvidenceLogit::new(f64::MAX).unwrap(),
        ];
        assert_eq!(
            fusion.fuse(&evidence),
            Err(BayesianEvidenceFusionError::NonFiniteEvidenceSum)
        );
    }
}
