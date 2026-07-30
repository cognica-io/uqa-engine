//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attention-based multi-signal fusion (Section 8, Paper 4).
//!
//! Computes per-signal attention weights from query features via a
//! learned `n_signals x n_query_features` matrix, softmax-normalizes,
//! and feeds them into a weighted log-odds conjunction. `fit` and
//! `update` train the matrix by gradient descent on the logistic loss
//! against ground-truth relevance labels.

use uqa_scoring::{logit, prob::log_odds_conjunction_weighted, sigmoid, PROB_EPSILON};

#[derive(Debug, Clone)]
pub struct AttentionFusion {
    pub n_signals: usize,
    pub n_query_features: usize,
    /// `[n_signals * n_query_features]`, row-major.
    pub weights: Vec<f64>,
    pub alpha: f64,
    /// Apply per-signal min-max normalization in logit space when fusing a
    /// batch of candidates. A single candidate has no population over which
    /// to normalize, so [`Self::fuse`] intentionally leaves it unchanged.
    pub normalize: bool,
    /// Corpus relevance prior added once in log-odds space after the
    /// confidence-scaled attention evidence.
    pub base_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttentionFusionState {
    pub n_signals: usize,
    pub n_query_features: usize,
    pub alpha: f64,
    pub weights_matrix: Vec<f64>,
    pub normalize: bool,
    pub base_rate: Option<f64>,
}

impl AttentionFusion {
    pub fn new(n_signals: usize, n_query_features: usize, alpha: f64) -> Self {
        // Preserve construction as an infallible model-description API,
        // but never let attacker-controlled dimensions overflow before
        // execution validation can report the malformed model.
        let weight_count = n_signals.checked_mul(n_query_features).unwrap_or(0);
        Self {
            n_signals,
            n_query_features,
            weights: vec![0.0; weight_count],
            alpha,
            normalize: false,
            base_rate: None,
        }
    }

    /// Configure SQL-visible attention options while preserving the
    /// infallible [`Self::new`] model-description API.
    pub fn with_options(
        mut self,
        normalize: bool,
        base_rate: Option<f64>,
    ) -> Result<Self, &'static str> {
        if base_rate.is_some_and(|rate| !rate.is_finite() || rate <= 0.0 || rate >= 1.0) {
            return Err("attention base_rate must be finite and in (0, 1)");
        }
        self.normalize = normalize;
        self.base_rate = base_rate;
        Ok(self)
    }

    /// Compute the per-signal attention vector from `query_features`
    /// (length `n_query_features`). Returns a softmax-normalized
    /// vector of length `n_signals`.
    pub fn attention_weights(&self, query_features: &[f64]) -> Result<Vec<f64>, &'static str> {
        self.validate_inputs(self.n_signals, query_features.len())?;
        if query_features.iter().any(|feature| !feature.is_finite()) {
            return Err("attention query features must be finite");
        }
        let mut raw = vec![0.0f64; self.n_signals];
        for (s, slot) in raw.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (f, feature) in query_features.iter().copied().enumerate() {
                acc += self.weights[s * self.n_query_features + f] * feature;
            }
            if !acc.is_finite() {
                return Err("attention logits overflowed the finite numeric range");
            }
            *slot = acc;
        }
        softmax(&raw)
    }

    pub fn validate_inputs(
        &self,
        signal_count: usize,
        query_feature_count: usize,
    ) -> Result<(), &'static str> {
        if signal_count == 0 || self.n_signals == 0 {
            return Err("attention fusion requires at least one signal");
        }
        if !self.alpha.is_finite() || !(0.0..=1.0).contains(&self.alpha) {
            return Err("attention alpha must be finite and in [0, 1]");
        }
        if signal_count != self.n_signals {
            return Err("attention signal count does not match the model");
        }
        if query_feature_count != self.n_query_features {
            return Err("attention query feature count does not match the model");
        }
        let expected_weights = self
            .n_signals
            .checked_mul(self.n_query_features)
            .ok_or("attention model dimensions overflow")?;
        if self.weights.len() != expected_weights {
            return Err("attention weight matrix length does not match the model dimensions");
        }
        if self.weights.iter().any(|weight| !weight.is_finite()) {
            return Err("attention weights must be finite");
        }
        if self
            .base_rate
            .is_some_and(|rate| !rate.is_finite() || rate <= 0.0 || rate >= 1.0)
        {
            return Err("attention base_rate must be finite and in (0, 1)");
        }
        Ok(())
    }

    fn validate_values(probs: &[f64], query_features: &[f64]) -> Result<(), &'static str> {
        if probs
            .iter()
            .any(|probability| !probability.is_finite() || !(0.0..=1.0).contains(probability))
        {
            return Err("attention probabilities must be finite and in [0, 1]");
        }
        if query_features.iter().any(|feature| !feature.is_finite()) {
            return Err("attention query features must be finite");
        }
        Ok(())
    }

    pub fn fuse(&self, probs: &[f64], query_features: &[f64]) -> Result<f64, &'static str> {
        self.validate_inputs(probs.len(), query_features.len())?;
        Self::validate_values(probs, query_features)?;
        if probs.len() == 1 {
            return Ok(self.apply_base_rate(probs[0]));
        }
        let weights = self.attention_weights(query_features)?;
        let probability = log_odds_conjunction_weighted(probs, &weights, self.alpha)?;
        Ok(self.apply_base_rate(probability))
    }

    /// Fuse an entire candidate batch. This is the physical operation needed
    /// by `normalized => true`: each signal column is independently min-max
    /// normalized in logit space across the candidates before attention
    /// weighting. A zero-variance column contributes zero normalized logit.
    pub fn fuse_batch(
        &self,
        probabilities: &[Vec<f64>],
        query_features: &[f64],
    ) -> Result<Vec<f64>, &'static str> {
        self.validate_inputs(self.n_signals, query_features.len())?;
        for sample in probabilities {
            self.validate_inputs(sample.len(), query_features.len())?;
            Self::validate_values(sample, query_features)?;
        }
        if probabilities.is_empty() {
            return Ok(Vec::new());
        }
        if !self.normalize || probabilities.len() == 1 {
            return probabilities
                .iter()
                .map(|sample| self.fuse(sample, query_features))
                .collect();
        }

        let weights = self.attention_weights(query_features)?;
        let mut normalized_logits = probabilities
            .iter()
            .map(|sample| sample.iter().copied().map(logit).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        for signal_index in 0..self.n_signals {
            let minimum = normalized_logits
                .iter()
                .map(|sample| sample[signal_index])
                .fold(f64::INFINITY, f64::min);
            let maximum = normalized_logits
                .iter()
                .map(|sample| sample[signal_index])
                .fold(f64::NEG_INFINITY, f64::max);
            let range = maximum - minimum;
            for sample in &mut normalized_logits {
                sample[signal_index] = if range < 1e-12 {
                    0.0
                } else {
                    (sample[signal_index] - minimum) / range
                };
            }
        }

        let scale = (self.n_signals as f64).powf(self.alpha);
        let prior_logit = self.base_rate.map_or(0.0, logit);
        normalized_logits
            .into_iter()
            .map(|sample| {
                let weighted_logit = sample
                    .iter()
                    .zip(&weights)
                    .map(|(value, weight)| value * weight)
                    .sum::<f64>();
                let probability = sigmoid(scale * weighted_logit + prior_logit);
                probability
                    .is_finite()
                    .then_some(probability)
                    .ok_or("attention fusion produced a non-finite probability")
            })
            .collect()
    }

    fn apply_base_rate(&self, probability: f64) -> f64 {
        self.base_rate.map_or(probability, |rate| {
            sigmoid(logit(probability) + logit(rate))
        })
    }

    pub fn state_dict(&self) -> AttentionFusionState {
        AttentionFusionState {
            n_signals: self.n_signals,
            n_query_features: self.n_query_features,
            alpha: self.alpha,
            weights_matrix: self.weights.clone(),
            normalize: self.normalize,
            base_rate: self.base_rate,
        }
    }

    pub fn load_state_dict(&mut self, state: &AttentionFusionState) -> Result<(), &'static str> {
        let candidate = Self {
            n_signals: state.n_signals,
            n_query_features: state.n_query_features,
            alpha: state.alpha,
            weights: state.weights_matrix.clone(),
            normalize: state.normalize,
            base_rate: state.base_rate,
        };
        candidate.validate_inputs(state.n_signals, state.n_query_features)?;
        *self = candidate;
        Ok(())
    }

    /// One SGD step on the logistic loss. Approximate gradient:
    /// `dL/dw_{s,f} ~ error * attention_s * logit(p_s) * query_feature_f`.
    pub fn update(
        &mut self,
        probs: &[f64],
        label: f64,
        query_features: &[f64],
        learning_rate: f64,
    ) -> Result<(), &'static str> {
        if !label.is_finite() || !(0.0..=1.0).contains(&label) {
            return Err("attention training label must be finite and in [0, 1]");
        }
        if !learning_rate.is_finite() || learning_rate < 0.0 {
            return Err("attention learning rate must be finite and non-negative");
        }
        let predicted = self.fuse(probs, query_features)?;
        let error = predicted - label;
        let attention = self.attention_weights(query_features)?;
        let mut next_weights = self.weights.clone();
        for s in 0..self.n_signals {
            let p = probs[s].clamp(PROB_EPSILON, 1.0 - PROB_EPSILON);
            let logit_p = (p / (1.0 - p)).ln();
            for (f, qf) in query_features.iter().copied().enumerate() {
                let grad = error * attention[s] * logit_p * qf;
                let index = s * self.n_query_features + f;
                let updated = self.weights[index] - learning_rate * grad;
                if !updated.is_finite() {
                    return Err("attention update produced a non-finite weight");
                }
                next_weights[index] = updated;
            }
        }
        self.weights = next_weights;
        Ok(())
    }

    pub fn fit(
        &mut self,
        probs: &[Vec<f64>],
        labels: &[f64],
        query_features: &[Vec<f64>],
        lr: f64,
        epochs: usize,
    ) -> Result<(), &'static str> {
        if probs.len() != labels.len() || probs.len() != query_features.len() {
            return Err(
                "attention training samples, labels, and query features must have equal lengths",
            );
        }
        if !lr.is_finite() || lr < 0.0 {
            return Err("attention learning rate must be finite and non-negative");
        }
        for ((sample, label), features) in probs.iter().zip(labels).zip(query_features) {
            if !label.is_finite() || !(0.0..=1.0).contains(label) {
                return Err("attention training labels must be finite and in [0, 1]");
            }
            self.validate_inputs(sample.len(), features.len())?;
            Self::validate_values(sample, features)?;
        }
        let mut candidate = self.clone();
        for _ in 0..epochs {
            for ((sample, &label), feats) in
                probs.iter().zip(labels.iter()).zip(query_features.iter())
            {
                candidate.update(sample, label, feats, lr)?;
            }
        }
        *self = candidate;
        Ok(())
    }
}

/// Multi-head attention fusion: average the per-head fused
/// log-odds for a more robust signal. Each head is an independent
/// [`AttentionFusion`].
#[derive(Debug, Clone)]
pub struct MultiHeadAttentionFusion {
    pub heads: Vec<AttentionFusion>,
}

impl MultiHeadAttentionFusion {
    pub fn new(n_heads: usize, n_signals: usize, n_query_features: usize, alpha: f64) -> Self {
        let heads = (0..n_heads)
            .map(|_| AttentionFusion::new(n_signals, n_query_features, alpha))
            .collect();
        Self { heads }
    }

    /// Checked constructor used for untrusted SQL options. It rejects an
    /// empty model and reports allocation failure instead of panicking or
    /// silently constructing a malformed fuser.
    pub fn try_new(
        n_heads: usize,
        n_signals: usize,
        n_query_features: usize,
        alpha: f64,
        normalize: bool,
    ) -> Result<Self, &'static str> {
        if n_heads == 0 {
            return Err("multi-head attention fusion requires at least one head");
        }
        let mut heads = Vec::new();
        heads
            .try_reserve_exact(n_heads)
            .map_err(|_| "multi-head attention head count exceeds available memory")?;
        for _ in 0..n_heads {
            heads.push(
                AttentionFusion::new(n_signals, n_query_features, alpha)
                    .with_options(normalize, None)?,
            );
        }
        Ok(Self { heads })
    }

    pub fn n_heads(&self) -> usize {
        self.heads.len()
    }

    pub fn normalize(&self) -> bool {
        self.heads
            .first()
            .is_some_and(|attention| attention.normalize)
    }

    pub fn alpha(&self) -> Option<f64> {
        self.heads.first().map(|attention| attention.alpha)
    }

    pub fn validate_inputs(
        &self,
        signal_count: usize,
        query_feature_count: usize,
    ) -> Result<(), &'static str> {
        if self.heads.is_empty() {
            return Err("multi-head attention fusion requires at least one head");
        }
        for head in &self.heads {
            head.validate_inputs(signal_count, query_feature_count)?;
        }
        Ok(())
    }

    pub fn fuse(&self, probs: &[f64], query_features: &[f64]) -> Result<f64, &'static str> {
        self.validate_inputs(probs.len(), query_features.len())?;
        let logits = self
            .heads
            .iter()
            .map(|h| h.fuse(probs, query_features))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(logit)
            .sum::<f64>();
        Ok(sigmoid(logits / self.heads.len() as f64))
    }

    pub fn fuse_batch(
        &self,
        probabilities: &[Vec<f64>],
        query_features: &[f64],
    ) -> Result<Vec<f64>, &'static str> {
        let signal_count = probabilities
            .first()
            .map(Vec::len)
            .or_else(|| self.heads.first().map(|head| head.n_signals))
            .ok_or("multi-head attention fusion requires at least one head")?;
        self.validate_inputs(signal_count, query_features.len())?;
        let head_results = self
            .heads
            .iter()
            .map(|head| head.fuse_batch(probabilities, query_features))
            .collect::<Result<Vec<_>, _>>()?;
        if probabilities.is_empty() {
            return Ok(Vec::new());
        }
        (0..probabilities.len())
            .map(|candidate_index| {
                let mean_logit = head_results
                    .iter()
                    .map(|results| logit(results[candidate_index]))
                    .sum::<f64>()
                    / self.heads.len() as f64;
                let probability = sigmoid(mean_logit);
                probability
                    .is_finite()
                    .then_some(probability)
                    .ok_or("multi-head attention fusion produced a non-finite probability")
            })
            .collect()
    }

    pub fn fit(
        &mut self,
        probs: &[Vec<f64>],
        labels: &[f64],
        query_features: &[Vec<f64>],
        lr: f64,
        epochs: usize,
    ) -> Result<(), &'static str> {
        for head in &mut self.heads {
            head.fit(probs, labels, query_features, lr, epochs)?;
        }
        Ok(())
    }
}

fn softmax(values: &[f64]) -> Result<Vec<f64>, &'static str> {
    if values.is_empty() {
        return Err("attention softmax requires at least one value");
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err("attention logits must be finite");
    }
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exp: Vec<f64> = values.iter().map(|v| (v - max).exp()).collect();
    let sum: f64 = exp.iter().sum();
    if !sum.is_finite() || sum <= 0.0 {
        return Err("attention softmax normalization is not finite and positive");
    }
    Ok(exp.into_iter().map(|v| v / sum).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_softmax_is_uniform_at_zero_weights() {
        let a = AttentionFusion::new(3, 4, 0.0);
        let weights = a
            .attention_weights(&[1.0, 0.5, 0.0, 1.0])
            .expect("valid attention shape");
        for w in &weights {
            assert!((w - 1.0 / 3.0).abs() < 1e-9);
        }
    }

    #[test]
    fn fuse_returns_input_when_single_signal() {
        let a = AttentionFusion::new(1, 4, 0.0);
        let p = a.fuse(&[0.83], &[0.5; 4]).expect("valid attention input");
        assert!((p - 0.83).abs() < 1e-9);
    }

    #[test]
    fn fit_increases_weight_on_informative_signal() {
        let mut a = AttentionFusion::new(2, 1, 0.0);
        let probs = vec![
            vec![0.9, 0.5],
            vec![0.9, 0.5],
            vec![0.1, 0.5],
            vec![0.1, 0.5],
        ];
        let labels = vec![1.0, 1.0, 0.0, 0.0];
        let qf = vec![vec![1.0]; 4];
        a.fit(&probs, &labels, &qf, 0.5, 200)
            .expect("valid training shapes");
        assert!(a.weights[0] > a.weights[1]);
    }

    #[test]
    fn multi_head_averages_predictions() {
        let mh = MultiHeadAttentionFusion::new(3, 2, 2, 0.0);
        let p = mh
            .fuse(&[0.7, 0.6], &[1.0, 0.0])
            .expect("valid multi-head input");
        // With zero weights and alpha=0, all heads return the same
        // mean-log-odds.
        assert!((0.0..=1.0).contains(&p));
    }

    #[test]
    fn base_rate_enters_once_as_an_additive_log_odds_prior() {
        let attention = AttentionFusion::new(2, 1, 0.5)
            .with_options(false, Some(0.2))
            .expect("valid base rate");
        let probability = attention
            .fuse(&[0.5, 0.5], &[0.0])
            .expect("neutral evidence fuses");
        assert!((probability - 0.2).abs() < 1e-12, "{probability}");
    }

    #[test]
    fn normalized_batch_uses_each_signal_candidate_range() {
        let attention = AttentionFusion::new(2, 1, 0.0)
            .with_options(true, None)
            .expect("normalization has no invalid parameters");
        let fused = attention
            .fuse_batch(&[vec![0.2, 0.4], vec![0.8, 0.6]], &[0.0])
            .expect("valid candidate batch");
        assert!((fused[0] - 0.5).abs() < 1e-12, "{}", fused[0]);
        assert!((fused[1] - sigmoid(1.0)).abs() < 1e-12, "{}", fused[1]);
    }

    #[test]
    fn multi_head_averages_in_log_odds_space() {
        let mut first = AttentionFusion::new(2, 1, 0.0);
        first.weights = vec![8.0, -8.0];
        let second = AttentionFusion::new(2, 1, 0.0);
        let multi_head = MultiHeadAttentionFusion {
            heads: vec![first.clone(), second.clone()],
        };
        let probabilities = [0.9, 0.2];
        let query_features = [1.0];
        let first_probability = first.fuse(&probabilities, &query_features).unwrap();
        let second_probability = second.fuse(&probabilities, &query_features).unwrap();
        let expected = sigmoid(f64::midpoint(
            logit(first_probability),
            logit(second_probability),
        ));
        let actual = multi_head
            .fuse(&probabilities, &query_features)
            .expect("valid multi-head model");
        assert!((actual - expected).abs() < 1e-12, "{actual} vs {expected}");
        assert!(
            (actual - f64::midpoint(first_probability, second_probability)).abs() > 1e-4,
            "regression: probabilities were averaged instead of logits"
        );
    }

    #[test]
    fn checked_attention_options_reject_invalid_models() {
        for base_rate in [0.0, 1.0, f64::NAN, f64::INFINITY] {
            assert!(AttentionFusion::new(2, 1, 0.5)
                .with_options(false, Some(base_rate))
                .is_err());
        }
        assert!(MultiHeadAttentionFusion::try_new(0, 2, 1, 0.5, false).is_err());
        let multi_head = MultiHeadAttentionFusion::try_new(4, 2, 1, 0.7, true)
            .expect("valid checked multi-head model");
        assert_eq!(multi_head.n_heads(), 4);
        assert!(multi_head.normalize());
        assert_eq!(multi_head.alpha(), Some(0.7));
    }

    #[test]
    fn invalid_numeric_inputs_and_training_shapes_are_errors() {
        let mut attention = AttentionFusion::new(2, 1, 0.5);
        assert!(attention.fuse(&[f64::NAN, 0.5], &[1.0]).is_err());
        assert!(attention.fuse(&[0.5, 0.5], &[f64::INFINITY]).is_err());
        assert!(attention
            .fit(&[vec![0.8, 0.2]], &[], &[vec![1.0]], 0.1, 1)
            .is_err());
        assert!(attention.update(&[0.8, 0.2], 2.0, &[1.0], 0.1).is_err());
    }

    #[test]
    fn failed_state_load_and_update_are_atomic() {
        let mut attention = AttentionFusion::new(2, 1, 0.5);
        let original = attention.state_dict();
        let invalid = AttentionFusionState {
            weights_matrix: vec![0.0],
            ..original.clone()
        };
        assert!(attention.load_state_dict(&invalid).is_err());
        assert_eq!(attention.state_dict(), original);

        assert!(attention
            .update(&[1.0, 0.0], 1.0, &[f64::MAX], f64::MAX)
            .is_err());
        assert_eq!(attention.state_dict(), original);
    }
}
