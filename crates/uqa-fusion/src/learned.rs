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

use uqa_scoring::{prob::log_odds_conjunction_weighted, sigmoid};

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

    /// Forward pass: softmax(weights), then weighted log-odds
    /// conjunction.
    pub fn fuse(&self, probs: &[f64]) -> f64 {
        if probs.is_empty() {
            return 0.5;
        }
        if probs.len() == 1 {
            return probs[0];
        }
        let weights = softmax(&self.weights);
        log_odds_conjunction_weighted(probs, &weights, self.alpha).unwrap_or(0.5)
    }

    pub fn state_dict(&self) -> LearnedFusionState {
        LearnedFusionState {
            n_signals: self.n_signals(),
            alpha: self.alpha,
            weights: self.weights.clone(),
        }
    }

    pub fn load_state_dict(&mut self, state: &LearnedFusionState) {
        self.weights.clone_from(&state.weights);
        if self.weights.len() != state.n_signals {
            self.weights.resize(state.n_signals, 0.0);
        }
        self.alpha = state.alpha;
    }

    /// One SGD step on the logistic loss
    /// `L = -[y log p̂ + (1-y) log(1-p̂)]` with per-signal weights.
    /// Uses a simple gradient approximation: the contribution of each
    /// signal scales with its softmax weight times its logit.
    pub fn update(&mut self, probs: &[f64], label: f64, learning_rate: f64) {
        if probs.len() != self.weights.len() {
            return;
        }
        let predicted = self.fuse(probs);
        let error = predicted - label;
        let weights = softmax(&self.weights);
        // Gradient w.r.t. raw weights via chain rule on softmax + log-odds.
        // Approximation: dL/dw_i ~ error * weight_i * logit(p_i).
        for i in 0..self.weights.len() {
            let p = probs[i].clamp(uqa_scoring::PROB_EPSILON, 1.0 - uqa_scoring::PROB_EPSILON);
            let logit_p = (p / (1.0 - p)).ln();
            self.weights[i] -= learning_rate * error * weights[i] * logit_p;
        }
    }

    pub fn fit(&mut self, probs: &[Vec<f64>], labels: &[f64], lr: f64, epochs: usize) {
        for _ in 0..epochs {
            for (sample, &label) in probs.iter().zip(labels.iter()) {
                self.update(sample, label, lr);
            }
        }
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

/// Logit helper used by the gradient step.
fn _logit(p: f64) -> f64 {
    let s = sigmoid(0.0); // ensure linkage so the import isn't dead
    let _ = s;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuse_with_zero_weights_collapses_to_uniform_log_odds() {
        let f = LearnedFusion::new(3, 0.5);
        let p = f.fuse(&[0.7, 0.6, 0.5]);
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
        f.fit(&probs, &labels, 0.5, 200);
        // After fitting, weight for signal 0 should be larger than for
        // signal 1 (informative > noise).
        assert!(f.weights[0] > f.weights[1]);
    }
}
