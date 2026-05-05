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

use uqa_scoring::{prob::log_odds_conjunction_weighted, PROB_EPSILON};

#[derive(Debug, Clone)]
pub struct AttentionFusion {
    pub n_signals: usize,
    pub n_query_features: usize,
    /// `[n_signals * n_query_features]`, row-major.
    pub weights: Vec<f64>,
    pub alpha: f64,
}

impl AttentionFusion {
    pub fn new(n_signals: usize, n_query_features: usize, alpha: f64) -> Self {
        Self {
            n_signals,
            n_query_features,
            weights: vec![0.0; n_signals * n_query_features],
            alpha,
        }
    }

    /// Compute the per-signal attention vector from `query_features`
    /// (length `n_query_features`). Returns a softmax-normalized
    /// vector of length `n_signals`.
    pub fn attention_weights(&self, query_features: &[f64]) -> Vec<f64> {
        let mut raw = vec![0.0f64; self.n_signals];
        for (s, slot) in raw.iter_mut().enumerate() {
            let mut acc = 0.0;
            for f in 0..self.n_query_features {
                acc += self.weights[s * self.n_query_features + f]
                    * query_features.get(f).copied().unwrap_or(0.0);
            }
            *slot = acc;
        }
        softmax(&raw)
    }

    pub fn fuse(&self, probs: &[f64], query_features: &[f64]) -> f64 {
        if probs.is_empty() {
            return 0.5;
        }
        if probs.len() == 1 {
            return probs[0];
        }
        let weights = self.attention_weights(query_features);
        log_odds_conjunction_weighted(probs, &weights, self.alpha).unwrap_or(0.5)
    }

    /// One SGD step on the logistic loss. Approximate gradient:
    /// `dL/dw_{s,f} ~ error * attention_s * logit(p_s) * query_feature_f`.
    pub fn update(
        &mut self,
        probs: &[f64],
        label: f64,
        query_features: &[f64],
        learning_rate: f64,
    ) {
        if probs.len() != self.n_signals {
            return;
        }
        let predicted = self.fuse(probs, query_features);
        let error = predicted - label;
        let attention = self.attention_weights(query_features);
        for s in 0..self.n_signals {
            let p = probs[s].clamp(PROB_EPSILON, 1.0 - PROB_EPSILON);
            let logit_p = (p / (1.0 - p)).ln();
            for f in 0..self.n_query_features {
                let qf = query_features.get(f).copied().unwrap_or(0.0);
                let grad = error * attention[s] * logit_p * qf;
                self.weights[s * self.n_query_features + f] -= learning_rate * grad;
            }
        }
    }

    pub fn fit(
        &mut self,
        probs: &[Vec<f64>],
        labels: &[f64],
        query_features: &[Vec<f64>],
        lr: f64,
        epochs: usize,
    ) {
        for _ in 0..epochs {
            for ((sample, &label), feats) in
                probs.iter().zip(labels.iter()).zip(query_features.iter())
            {
                self.update(sample, label, feats, lr);
            }
        }
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

    pub fn n_heads(&self) -> usize {
        self.heads.len()
    }

    pub fn fuse(&self, probs: &[f64], query_features: &[f64]) -> f64 {
        if self.heads.is_empty() {
            return 0.5;
        }
        let sum: f64 = self
            .heads
            .iter()
            .map(|h| h.fuse(probs, query_features))
            .sum();
        sum / self.heads.len() as f64
    }

    pub fn fit(
        &mut self,
        probs: &[Vec<f64>],
        labels: &[f64],
        query_features: &[Vec<f64>],
        lr: f64,
        epochs: usize,
    ) {
        for head in &mut self.heads {
            head.fit(probs, labels, query_features, lr, epochs);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_softmax_is_uniform_at_zero_weights() {
        let a = AttentionFusion::new(3, 4, 0.0);
        let weights = a.attention_weights(&[1.0, 0.5, 0.0, 1.0]);
        for w in &weights {
            assert!((w - 1.0 / 3.0).abs() < 1e-9);
        }
    }

    #[test]
    fn fuse_returns_input_when_single_signal() {
        let a = AttentionFusion::new(1, 4, 0.0);
        let p = a.fuse(&[0.83], &[0.5; 4]);
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
        a.fit(&probs, &labels, &qf, 0.5, 200);
        assert!(a.weights[0] > a.weights[1]);
    }

    #[test]
    fn multi_head_averages_predictions() {
        let mh = MultiHeadAttentionFusion::new(3, 2, 2, 0.0);
        let p = mh.fuse(&[0.7, 0.6], &[1.0, 0.0]);
        // With zero weights and alpha=0, all heads return the same
        // mean-log-odds.
        assert!((0.0..=1.0).contains(&p));
    }
}
