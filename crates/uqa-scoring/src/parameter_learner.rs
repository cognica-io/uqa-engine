//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supervised fitting for query-level Bayesian BM25 calibration.
//!
//! The fitted posterior is the same transform used by
//! `BayesianBM25Scorer`: `sigmoid(alpha * (raw_bm25_score - beta))`.
//! Any intercept the labels call for is absorbed into `beta`. The
//! `base_rate` is not a model term: it tracks the observed positive
//! label rate as an exponential moving average, estimating the corpus
//! relevance prior that fusion applies exactly once.

use std::collections::BTreeMap;

use crate::error::{invalid_input, require_finite, require_probability};
use crate::prob::sigmoid;
use crate::{ScoringError, ScoringResult};

#[derive(Debug, Clone)]
pub struct ParameterLearner {
    alpha: f64,
    beta: f64,
    base_rate: Option<f64>,
}

impl ParameterLearner {
    pub fn new(alpha: f64, beta: f64, base_rate: Option<f64>) -> ScoringResult<Self> {
        require_finite(alpha, "alpha")?;
        if alpha <= 0.0 {
            return Err(invalid_input(format!(
                "alpha must be positive, got {alpha}"
            )));
        }
        require_finite(beta, "beta")?;
        if let Some(base_rate) = base_rate {
            require_probability(base_rate, "base_rate")?;
            if base_rate == 0.0 || base_rate == 1.0 {
                return Err(invalid_input(format!(
                    "base_rate must be strictly between 0 and 1, got {base_rate}"
                )));
            }
        }
        Ok(Self {
            alpha,
            beta,
            base_rate,
        })
    }

    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    pub fn beta(&self) -> f64 {
        self.beta
    }

    pub fn base_rate(&self) -> Option<f64> {
        self.base_rate
    }

    pub fn params(&self) -> BTreeMap<String, f64> {
        BTreeMap::from([
            ("alpha".to_string(), self.alpha),
            ("beta".to_string(), self.beta),
            ("base_rate".to_string(), self.base_rate.unwrap_or(0.0)),
        ])
    }

    pub fn probability(&self, raw_score: f64) -> ScoringResult<f64> {
        require_finite(raw_score, "raw_score")?;
        let centered = raw_score - self.beta;
        let logit = self.alpha * centered;
        if !centered.is_finite() || !logit.is_finite() {
            return Err(ScoringError::ArithmeticOverflow(format!(
                "learner logit is not finite for score {raw_score}"
            )));
        }
        Ok(sigmoid(logit))
    }

    /// Apply one exact logistic-loss gradient step to a raw BM25 score.
    pub fn update(&mut self, raw_score: f64, label: f64, learning_rate: f64) -> ScoringResult<()> {
        require_finite(raw_score, "raw_score")?;
        require_probability(label, "label")?;
        require_finite(learning_rate, "learning_rate")?;
        if learning_rate <= 0.0 {
            return Err(invalid_input(format!(
                "learning_rate must be positive, got {learning_rate}"
            )));
        }

        let error = self.probability(raw_score)? - label;
        let alpha_before = self.alpha;
        let beta_before = self.beta;
        let next_alpha = self.alpha - learning_rate * error * (raw_score - beta_before);
        let next_beta = self.beta - learning_rate * error * -alpha_before;
        if !next_alpha.is_finite() || next_alpha <= 0.0 || !next_beta.is_finite() {
            return Err(ScoringError::ArithmeticOverflow(format!(
                "gradient update produced invalid parameters alpha={next_alpha}, beta={next_beta}"
            )));
        }

        let next_base_rate = if let Some(base_rate) = self.base_rate {
            let updated = base_rate + learning_rate * (label - base_rate);
            if !updated.is_finite() || updated <= 0.0 || updated >= 1.0 {
                return Err(ScoringError::ArithmeticOverflow(format!(
                    "gradient update produced invalid base_rate={updated}"
                )));
            }
            Some(updated)
        } else {
            None
        };

        self.alpha = next_alpha;
        self.beta = next_beta;
        self.base_rate = next_base_rate;
        Ok(())
    }

    pub fn fit(
        &mut self,
        raw_scores: &[f64],
        labels: &[f64],
        learning_rate: f64,
        epochs: usize,
    ) -> ScoringResult<BTreeMap<String, f64>> {
        if raw_scores.len() != labels.len() {
            return Err(invalid_input(format!(
                "raw_scores length {} does not match labels length {}",
                raw_scores.len(),
                labels.len()
            )));
        }
        require_finite(learning_rate, "learning_rate")?;
        if learning_rate <= 0.0 {
            return Err(invalid_input(format!(
                "learning_rate must be positive, got {learning_rate}"
            )));
        }
        for (index, raw_score) in raw_scores.iter().copied().enumerate() {
            require_finite(raw_score, &format!("raw_scores[{index}]"))?;
        }
        for (index, label) in labels.iter().copied().enumerate() {
            require_probability(label, &format!("labels[{index}]"))?;
        }

        let mut candidate = self.clone();
        for _ in 0..epochs {
            for (&raw_score, &label) in raw_scores.iter().zip(labels) {
                candidate.update(raw_score, label, learning_rate)?;
            }
        }
        *self = candidate;
        Ok(self.params())
    }

    pub fn fit_with_options(
        &mut self,
        raw_scores: &[f64],
        labels: &[f64],
    ) -> ScoringResult<BTreeMap<String, f64>> {
        self.fit(raw_scores, labels, 0.1, 50)
    }
}

impl Default for ParameterLearner {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            beta: 0.0,
            base_rate: Some(0.5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_sharpens_alpha_for_separable_labels() {
        let mut learner = ParameterLearner::new(0.5, 0.0, None).unwrap();
        let scores: Vec<f64> = (0_usize..40)
            .map(|i| if i.is_multiple_of(2) { 5.0 } else { -5.0 })
            .collect();
        let labels: Vec<f64> = (0_usize..40)
            .map(|i| if i.is_multiple_of(2) { 1.0 } else { 0.0 })
            .collect();
        let alpha_before = learner.alpha();
        learner.fit(&scores, &labels, 0.5, 50).unwrap();
        assert!(
            learner.alpha() > alpha_before,
            "alpha {} -> {}",
            alpha_before,
            learner.alpha()
        );
    }

    #[test]
    fn update_reduces_logistic_loss() {
        let mut learner = ParameterLearner::new(1.0, 0.0, Some(0.5)).unwrap();
        let before = -learner.probability(2.0).unwrap().ln();
        learner.update(2.0, 1.0, 0.1).unwrap();
        let after = -learner.probability(2.0).unwrap().ln();
        assert!(after < before, "loss {before} -> {after}");
    }

    #[test]
    fn invalid_updates_leave_parameters_unchanged() {
        let mut learner = ParameterLearner::default();
        let before = learner.params();
        assert!(learner.update(f64::NAN, 1.0, 0.1).is_err());
        assert_eq!(learner.params(), before);

        assert!(learner.fit(&[1.0, 2.0], &[1.0], 0.1, 1).is_err());
        assert_eq!(learner.params(), before);
    }
}
