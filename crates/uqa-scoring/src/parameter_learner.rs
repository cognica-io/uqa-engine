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

use crate::prob::{sigmoid, PROB_EPSILON};

#[derive(Debug, Clone)]
pub struct ParameterLearner {
    alpha: f64,
    beta: f64,
    base_rate: Option<f64>,
}

impl ParameterLearner {
    pub fn new(alpha: f64, beta: f64, base_rate: Option<f64>) -> Self {
        assert!(
            alpha.is_finite() && alpha > 0.0,
            "alpha must be a positive finite value, got {alpha}"
        );
        assert!(beta.is_finite(), "beta must be finite, got {beta}");
        if let Some(base_rate) = base_rate {
            assert!(
                base_rate.is_finite() && base_rate > 0.0 && base_rate < 1.0,
                "base_rate must be in (0, 1), got {base_rate}"
            );
        }
        Self {
            alpha,
            beta,
            base_rate,
        }
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

    pub fn probability(&self, raw_score: f64) -> f64 {
        sigmoid(self.alpha * (raw_score - self.beta))
    }

    /// Apply one exact logistic-loss gradient step to a raw BM25 score.
    pub fn update(&mut self, raw_score: f64, label: f64, learning_rate: f64) {
        assert!(raw_score.is_finite(), "raw_score must be finite");
        assert!(
            label.is_finite() && (0.0..=1.0).contains(&label),
            "label must be in [0, 1], got {label}"
        );
        assert!(
            learning_rate.is_finite() && learning_rate > 0.0,
            "learning_rate must be positive and finite"
        );

        let error = self.probability(raw_score) - label;
        let alpha_before = self.alpha;
        let beta_before = self.beta;
        self.alpha =
            (self.alpha - learning_rate * error * (raw_score - beta_before)).max(PROB_EPSILON);
        self.beta -= learning_rate * error * -alpha_before;

        if let Some(base_rate) = self.base_rate {
            let updated = base_rate + learning_rate * (label - base_rate);
            self.base_rate = Some(updated.clamp(PROB_EPSILON, 1.0 - PROB_EPSILON));
        }
    }

    pub fn fit(
        &mut self,
        raw_scores: &[f64],
        labels: &[f64],
        learning_rate: f64,
        epochs: usize,
    ) -> BTreeMap<String, f64> {
        assert_eq!(
            raw_scores.len(),
            labels.len(),
            "raw_scores and labels must have equal lengths"
        );
        for _ in 0..epochs {
            for (&raw_score, &label) in raw_scores.iter().zip(labels) {
                self.update(raw_score, label, learning_rate);
            }
        }
        self.params()
    }

    pub fn fit_with_options(
        &mut self,
        raw_scores: &[f64],
        labels: &[f64],
    ) -> BTreeMap<String, f64> {
        self.fit(raw_scores, labels, 0.1, 50)
    }
}

impl Default for ParameterLearner {
    fn default() -> Self {
        Self::new(1.0, 0.0, Some(0.5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_sharpens_alpha_for_separable_labels() {
        let mut learner = ParameterLearner::new(0.5, 0.0, None);
        let scores: Vec<f64> = (0..40)
            .map(|i| if i % 2 == 0 { 5.0 } else { -5.0 })
            .collect();
        let labels: Vec<f64> = (0..40)
            .map(|i| if i % 2 == 0 { 1.0 } else { 0.0 })
            .collect();
        let alpha_before = learner.alpha();
        learner.fit(&scores, &labels, 0.5, 50);
        assert!(
            learner.alpha() > alpha_before,
            "alpha {} -> {}",
            alpha_before,
            learner.alpha()
        );
    }

    #[test]
    fn update_reduces_logistic_loss() {
        let mut learner = ParameterLearner::new(1.0, 0.0, Some(0.5));
        let before = -learner.probability(2.0).ln();
        learner.update(2.0, 1.0, 0.1);
        let after = -learner.probability(2.0).ln();
        assert!(after < before, "loss {before} -> {after}");
    }
}
