//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Semantic score-domain types.
//!
//! The scoring pipeline uses several numerically identical `f64` values with
//! different algebraic meanings. These wrappers make the conversions explicit
//! at public mathematical boundaries so raw BM25 scores, evidence logits,
//! priors, and posterior probabilities cannot be combined accidentally.

use crate::error::{require_finite, require_probability};
use crate::prob::{logit, sigmoid};
use crate::ScoringResult;

/// An uncalibrated, complete BM25 query score.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct RawBm25Score(f64);

impl RawBm25Score {
    pub fn new(value: f64) -> ScoringResult<Self> {
        require_finite(value, "raw BM25 score")?;
        Ok(Self(value))
    }

    pub const fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for RawBm25Score {
    type Error = crate::ScoringError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RawBm25Score> for f64 {
    fn from(score: RawBm25Score) -> Self {
        score.value()
    }
}

/// Signed prior-free log-likelihood-ratio evidence.
///
/// Zero is neutral. Positive values support relevance and negative values
/// oppose it.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct EvidenceLogit(f64);

impl EvidenceLogit {
    pub fn new(value: f64) -> ScoringResult<Self> {
        require_finite(value, "evidence logit")?;
        Ok(Self(value))
    }

    /// Convert a prior-free probability-like evidence value into logit space.
    ///
    /// Calling this method is an explicit assertion that `probability` does
    /// not already contain a relevance prior.
    pub fn from_prior_free_probability(probability: f64) -> ScoringResult<Self> {
        require_probability(probability, "prior-free evidence probability")?;
        Ok(Self(logit(probability)))
    }

    pub const fn neutral() -> Self {
        Self(0.0)
    }

    pub const fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for EvidenceLogit {
    type Error = crate::ScoringError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<EvidenceLogit> for f64 {
    fn from(logit: EvidenceLogit) -> Self {
        logit.value()
    }
}

/// A relevance prior represented in log-odds space.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct PriorLogit(f64);

impl PriorLogit {
    pub fn new(value: f64) -> ScoringResult<Self> {
        require_finite(value, "prior logit")?;
        Ok(Self(value))
    }

    pub fn from_probability(probability: f64) -> ScoringResult<Self> {
        require_probability(probability, "prior probability")?;
        if probability == 0.0 || probability == 1.0 {
            return Err(crate::ScoringError::InvalidInput(format!(
                "prior probability must be strictly between 0 and 1, got {probability}"
            )));
        }
        Ok(Self(logit(probability)))
    }

    pub const fn neutral() -> Self {
        Self(0.0)
    }

    pub const fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for PriorLogit {
    type Error = crate::ScoringError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<PriorLogit> for f64 {
    fn from(logit: PriorLogit) -> Self {
        logit.value()
    }
}

/// A probability in the closed unit interval that is explicitly interpreted
/// as a posterior relevance probability.
///
/// The wrapper enforces the numeric and algebraic domain; it does not certify
/// empirical calibration. Parameters fitted without labels must still be
/// evaluated on held-out judgments before their outputs are described as
/// calibrated probabilities.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct PosteriorProbability(f64);

impl PosteriorProbability {
    pub fn new(value: f64) -> ScoringResult<Self> {
        require_probability(value, "posterior probability")?;
        Ok(Self(value))
    }

    pub fn from_logit(value: f64) -> ScoringResult<Self> {
        require_finite(value, "posterior logit")?;
        Self::new(sigmoid(value))
    }

    pub const fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for PosteriorProbability {
    type Error = crate::ScoringError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<PosteriorProbability> for f64 {
    fn from(probability: PosteriorProbability) -> Self {
        probability.value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_constructors_reject_cross_domain_invalid_values() {
        assert!(RawBm25Score::new(f64::NAN).is_err());
        assert!(EvidenceLogit::new(f64::INFINITY).is_err());
        assert!(PriorLogit::from_probability(0.0).is_err());
        assert!(PriorLogit::from_probability(1.0).is_err());
        assert!(PosteriorProbability::new(-0.1).is_err());
        assert!(PosteriorProbability::new(1.1).is_err());
    }

    #[test]
    fn neutral_evidence_and_prior_have_zero_logit() {
        assert_eq!(EvidenceLogit::neutral().value(), 0.0);
        assert_eq!(PriorLogit::neutral().value(), 0.0);
        assert_eq!(
            EvidenceLogit::from_prior_free_probability(0.5)
                .unwrap()
                .value(),
            0.0
        );
        assert_eq!(PriorLogit::from_probability(0.5).unwrap().value(), 0.0);
    }

    #[test]
    fn posterior_logit_conversion_is_explicit() {
        let posterior = PosteriorProbability::from_logit(0.0).unwrap();
        assert_eq!(posterior.value(), 0.5);
    }
}
