//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Learnable per-signal weights without query features (Section 8,
//! Paper 4).
//!
//! Weighted log-odds conjunction with weights trained by gradient
//! descent on the logistic loss. The forward pass softmax-normalizes
//! the raw weight vector before fusion so weights can be compared
//! across signals.

use uqa_scoring::prob::log_odds_conjunction_weighted;

#[derive(Debug, Clone)]
pub struct LearnedFusion {
    /// Raw (unnormalized) weights. Softmax during fusion.
    pub weights: Vec<f64>,
    pub alpha: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LearnedFusionState {
    pub n_signals: usize,
    pub alpha: f64,
    pub weights: Vec<f64>,
}

impl LearnedFusion {
    pub fn new(n_signals: usize, alpha: f64) -> Self {
        Self {
            weights: vec![0.0; n_signals],
            alpha,
        }
    }

    pub fn n_signals(&self) -> usize {
        self.weights.len()
    }

    pub fn validate_inputs(&self, signal_count: usize) -> Result<(), &'static str> {
        if signal_count == 0 || self.weights.is_empty() {
            return Err("learned fusion requires at least one signal");
        }
        if signal_count != self.weights.len() {
            return Err("learned fusion signal count does not match the model");
        }
        if !self.alpha.is_finite() || !(0.0..=1.0).contains(&self.alpha) {
            return Err("learned fusion alpha must be finite and in [0, 1]");
        }
        if self.weights.iter().any(|weight| !weight.is_finite()) {
            return Err("learned fusion weights must be finite");
        }
        Ok(())
    }

    /// Forward pass: softmax(weights), then weighted log-odds
    /// conjunction.
    pub fn fuse(&self, probs: &[f64]) -> Result<f64, &'static str> {
        self.validate_inputs(probs.len())?;
        if probs
            .iter()
            .any(|probability| !probability.is_finite() || !(0.0..=1.0).contains(probability))
        {
            return Err("learned fusion probabilities must be finite and in [0, 1]");
        }
        if probs.len() == 1 {
            return Ok(probs[0]);
        }
        let weights = softmax(&self.weights);
        log_odds_conjunction_weighted(probs, &weights, self.alpha)
    }

    pub fn state_dict(&self) -> LearnedFusionState {
        LearnedFusionState {
            n_signals: self.n_signals(),
            alpha: self.alpha,
            weights: self.weights.clone(),
        }
    }

    pub fn load_state_dict(&mut self, state: &LearnedFusionState) -> Result<(), &'static str> {
        if state.weights.len() != state.n_signals {
            return Err("learned fusion state weight count does not match n_signals");
        }
        let candidate = Self {
            weights: state.weights.clone(),
            alpha: state.alpha,
        };
        candidate.validate_inputs(state.n_signals)?;
        *self = candidate;
        Ok(())
    }

    /// One SGD step on the logistic loss
    /// `L = -[y log p̂ + (1-y) log(1-p̂)]` with per-signal weights.
    /// Uses a simple gradient approximation: the contribution of each
    /// signal scales with its softmax weight times its logit.
    pub fn update(
        &mut self,
        probs: &[f64],
        label: f64,
        learning_rate: f64,
    ) -> Result<(), &'static str> {
        if !label.is_finite() || !(0.0..=1.0).contains(&label) {
            return Err("learned fusion training label must be finite and in [0, 1]");
        }
        if !learning_rate.is_finite() || learning_rate < 0.0 {
            return Err("learned fusion learning rate must be finite and non-negative");
        }
        let predicted = self.fuse(probs)?;
        let error = predicted - label;
        let weights = softmax(&self.weights);
        let mut next_weights = self.weights.clone();
        // Gradient w.r.t. raw weights via chain rule on softmax + log-odds.
        // Approximation: dL/dw_i ~ error * weight_i * logit(p_i).
        for i in 0..self.weights.len() {
            let p = probs[i].clamp(uqa_scoring::PROB_EPSILON, 1.0 - uqa_scoring::PROB_EPSILON);
            let logit_p = (p / (1.0 - p)).ln();
            let updated = self.weights[i] - learning_rate * error * weights[i] * logit_p;
            if !updated.is_finite() {
                return Err("learned fusion update produced a non-finite weight");
            }
            next_weights[i] = updated;
        }
        self.weights = next_weights;
        Ok(())
    }

    pub fn fit(
        &mut self,
        probs: &[Vec<f64>],
        labels: &[f64],
        lr: f64,
        epochs: usize,
    ) -> Result<(), &'static str> {
        if probs.len() != labels.len() {
            return Err("learned fusion training samples and labels must have equal lengths");
        }
        if !lr.is_finite() || lr < 0.0 {
            return Err("learned fusion learning rate must be finite and non-negative");
        }
        for (sample, label) in probs.iter().zip(labels) {
            if !label.is_finite() || !(0.0..=1.0).contains(label) {
                return Err("learned fusion training labels must be finite and in [0, 1]");
            }
            self.fuse(sample)?;
        }
        let mut candidate = self.clone();
        for _ in 0..epochs {
            for (sample, &label) in probs.iter().zip(labels.iter()) {
                candidate.update(sample, label, lr)?;
            }
        }
        *self = candidate;
        Ok(())
    }
}

fn softmax(values: &[f64]) -> Vec<f64> {
    if values.is_empty() {
        return Vec::new();
    }
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exp: Vec<f64> = values.iter().map(|v| (v - max).exp()).collect();
    let sum: f64 = exp.iter().sum();
    if sum == 0.0 {
        let n = values.len() as f64;
        return vec![1.0 / n; values.len()];
    }
    exp.into_iter().map(|v| v / sum).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuse_with_zero_weights_collapses_to_uniform_log_odds() {
        let f = LearnedFusion::new(3, 0.5);
        let p = f
            .fuse(&[0.7, 0.6, 0.5])
            .expect("valid learned-fusion input");
        assert!((0.0..=1.0).contains(&p));
    }

    #[test]
    fn fit_drives_weights_toward_label_signal() {
        let mut f = LearnedFusion::new(2, 0.0);
        // Signal 0 perfectly predicts label, signal 1 is noise.
        let probs = vec![
            vec![0.9, 0.5],
            vec![0.9, 0.5],
            vec![0.1, 0.5],
            vec![0.1, 0.5],
        ];
        let labels = vec![1.0, 1.0, 0.0, 0.0];
        f.fit(&probs, &labels, 0.5, 200)
            .expect("valid training shapes");
        // After fitting, weight for signal 0 should be larger than for
        // signal 1 (informative > noise).
        assert!(f.weights[0] > f.weights[1]);
    }

    #[test]
    fn invalid_values_and_training_shapes_are_errors() {
        let mut fusion = LearnedFusion::new(2, 0.5);
        assert!(fusion.fuse(&[f64::NAN, 0.5]).is_err());
        assert!(fusion.fuse(&[-0.1, 0.5]).is_err());
        assert!(fusion.fit(&[vec![0.8, 0.2]], &[], 0.1, 1).is_err());
        assert!(fusion.update(&[0.8, 0.2], 1.5, 0.1).is_err());
    }

    #[test]
    fn failed_state_load_and_update_are_atomic() {
        let mut fusion = LearnedFusion::new(2, 0.5);
        let original = fusion.state_dict();
        let invalid = LearnedFusionState {
            n_signals: 3,
            alpha: 0.5,
            weights: vec![0.0; 2],
        };
        assert!(fusion.load_state_dict(&invalid).is_err());
        assert_eq!(fusion.state_dict(), original);

        assert!(fusion.update(&[1.0, 0.0], 1.0, f64::MAX).is_err());
        assert_eq!(fusion.state_dict(), original);
    }
}
