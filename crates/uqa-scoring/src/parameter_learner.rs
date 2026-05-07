//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Online and batch parameter learning for [`BayesianProbabilityTransform`]
//! (Section 8, Paper 3).
//!
//! Treats `(score, label)` pairs as Bernoulli observations whose
//! probability comes from `score_to_probability(score, tf, doc_len_ratio)`.
//! Each call to `update` does one gradient-descent step on the
//! logistic loss with respect to `alpha`, `beta`, and (optionally)
//! `base_rate`. `fit` runs multiple epochs over a batch.

use std::collections::BTreeMap;

use crate::bayesian::BayesianProbabilityTransform;
use crate::prob::PROB_EPSILON;

#[derive(Debug, Clone)]
pub struct ParameterLearner {
    pub transform: BayesianProbabilityTransform,
}

impl ParameterLearner {
    pub fn new(alpha: f64, beta: f64, base_rate: Option<f64>) -> Self {
        Self {
            transform: BayesianProbabilityTransform::new(alpha, beta, base_rate),
        }
    }

    pub fn from_transform(transform: BayesianProbabilityTransform) -> Self {
        Self { transform }
    }

    pub fn alpha(&self) -> f64 {
        self.transform.alpha
    }

    pub fn beta(&self) -> f64 {
        self.transform.beta
    }

    pub fn base_rate(&self) -> Option<f64> {
        self.transform.base_rate
    }

    pub fn params(&self) -> BTreeMap<String, f64> {
        let mut out = BTreeMap::new();
        out.insert("alpha".into(), self.transform.alpha);
        out.insert("beta".into(), self.transform.beta);
        out.insert("base_rate".into(), self.transform.base_rate.unwrap_or(0.5));
        out
    }

    /// Single-observation gradient step on the logistic loss.
    ///
    /// `tf` and `doc_len_ratio` parameterize the composite prior; pass
    /// `1.0` for `tf` and `1.0` for `doc_len_ratio` when the caller
    /// only knows the BM25 score.
    pub fn update(
        &mut self,
        score: f64,
        label: f64,
        tf: f64,
        doc_len_ratio: f64,
        learning_rate: f64,
    ) {
        let pred = self
            .transform
            .score_to_probability(score, tf, doc_len_ratio)
            .clamp(PROB_EPSILON, 1.0 - PROB_EPSILON);
        let err = pred - label;
        // The likelihood is sigma(alpha*(score - beta)). Probability is a
        // chained Bayes update on top of that. The chain rule across the
        // posterior is messy in closed form, so we use the standard
        // gradient surrogate for sigmoidal classifiers — treat
        // `dP/dlikelihood ≈ 1` and propagate through the linear
        // pre-activation `z = alpha * (score - beta)`.
        let l = self.transform.likelihood(score);
        let grad_z = err * l * (1.0 - l);
        let grad_alpha = grad_z * (score - self.transform.beta);
        let grad_beta = -grad_z * self.transform.alpha;
        self.transform.alpha -= learning_rate * grad_alpha;
        self.transform.beta -= learning_rate * grad_beta;
        if let Some(br) = self.transform.base_rate {
            let grad_br = err;
            let new_br = (br - learning_rate * grad_br).clamp(PROB_EPSILON, 1.0 - PROB_EPSILON);
            self.transform.base_rate = Some(new_br);
        }
    }

    /// Batch gradient descent over `epochs` passes through `(scores,
    /// labels)`. `tfs` and `doc_len_ratios` may be `None` to default
    /// each observation's prior parameters to `(1.0, 1.0)`.
    pub fn fit(
        &mut self,
        scores: &[f64],
        labels: &[f64],
        tfs: Option<&[f64]>,
        doc_len_ratios: Option<&[f64]>,
        learning_rate: f64,
        epochs: usize,
    ) -> BTreeMap<String, f64> {
        for _ in 0..epochs {
            for i in 0..scores.len() {
                let tf = tfs.and_then(|t| t.get(i).copied()).unwrap_or(1.0);
                let dlr = doc_len_ratios
                    .and_then(|t| t.get(i).copied())
                    .unwrap_or(1.0);
                self.update(scores[i], labels[i], tf, dlr, learning_rate);
            }
        }
        self.params()
    }

    pub fn fit_with_options(
        &mut self,
        scores: &[f64],
        labels: &[f64],
        tfs: Option<&[f64]>,
        doc_len_ratios: Option<&[f64]>,
    ) -> BTreeMap<String, f64> {
        self.fit(scores, labels, tfs, doc_len_ratios, 0.1, 50)
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
        learner.fit(&scores, &labels, None, None, 0.5, 50);
        // For score=+5/label=1 and score=-5/label=0, gradient ascent
        // should raise alpha (sharper sigmoid).
        assert!(
            learner.alpha() > alpha_before,
            "alpha {} -> {}",
            alpha_before,
            learner.alpha()
        );
    }

    #[test]
    fn update_changes_parameters() {
        let mut learner = ParameterLearner::new(1.0, 0.0, Some(0.5));
        let alpha0 = learner.alpha();
        let beta0 = learner.beta();
        learner.update(2.0, 1.0, 5.0, 1.0, 0.1);
        assert!(learner.alpha() != alpha0 || learner.beta() != beta0);
    }
}
