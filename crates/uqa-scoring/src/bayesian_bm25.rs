//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-level Bayesian calibration for BM25 scores.

use std::sync::Arc;

use uqa_core::IndexStats;

use crate::bm25::{BM25Params, BM25Scorer};
use crate::prob::{logit, sigmoid};

#[derive(Debug, Clone, Copy)]
pub struct BayesianBM25Params {
    pub bm25: BM25Params,
    pub alpha: f64,
    pub beta: f64,
    /// Corpus base rate in `[0, 1)`. Zero disables the base-rate shift.
    pub base_rate: f64,
}

impl Default for BayesianBM25Params {
    fn default() -> Self {
        Self {
            bm25: BM25Params::default(),
            alpha: 1.0,
            beta: 0.0,
            base_rate: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BayesianBM25Scorer {
    pub params: BayesianBM25Params,
    pub bm25: BM25Scorer,
    logit_base_rate: f64,
}

impl BayesianBM25Scorer {
    pub fn new(params: BayesianBM25Params, stats: Arc<IndexStats>) -> Self {
        assert!(
            params.alpha.is_finite() && params.alpha > 0.0,
            "alpha must be a positive finite value, got {}",
            params.alpha
        );
        assert!(
            params.beta.is_finite(),
            "beta must be a finite value, got {}",
            params.beta
        );
        assert!(
            params.base_rate.is_finite() && params.base_rate >= 0.0 && params.base_rate < 1.0,
            "base_rate must be in [0, 1), got {}",
            params.base_rate
        );
        let logit_base_rate = if params.base_rate > 0.0 {
            logit(params.base_rate)
        } else {
            0.0
        };
        Self {
            params,
            bm25: BM25Scorer::new(params.bm25, stats),
            logit_base_rate,
        }
    }

    pub fn idf(&self, doc_freq: u64) -> f64 {
        self.bm25.idf(doc_freq)
    }

    /// Score a one-term query and calibrate the complete BM25 score once.
    pub fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64 {
        let idf_val = self.bm25.idf(doc_freq);
        self.score_with_idf(term_freq, doc_length, idf_val)
    }

    pub fn score_with_idf(&self, term_freq: u64, doc_length: u64, idf_val: f64) -> f64 {
        let raw = self.bm25.score_with_idf(term_freq, doc_length, idf_val);
        self.calibrate_raw_score(raw)
    }

    /// Calibrate the complete raw BM25 query score.
    ///
    /// `P = sigmoid(alpha * (score - beta) + logit(base_rate))`.
    /// The base-rate term is zero when `base_rate` is disabled.
    pub fn calibrate_raw_score(&self, raw_score: f64) -> f64 {
        sigmoid(self.params.alpha * (raw_score - self.params.beta) + self.logit_base_rate)
    }

    /// Combine raw BM25 term contributions, then calibrate exactly once.
    pub fn combine_scores(&self, raw_term_scores: &[f64]) -> f64 {
        self.calibrate_raw_score(BM25Scorer::combine_scores(raw_term_scores))
    }

    /// Bayesian WAND upper bound (Theorem 6.1.2): tightest safe pruning
    /// bound derived from the BM25 supremum and the maximum prior.
    pub fn upper_bound(&self, doc_freq: u64) -> f64 {
        let bm25_ub = self.bm25.upper_bound(doc_freq);
        self.calibrate_raw_score(bm25_ub)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(n: u64, avgdl: f64) -> Arc<IndexStats> {
        let mut s = IndexStats::default();
        s.total_docs = n;
        s.avg_doc_length = avgdl;
        Arc::new(s)
    }

    #[test]
    fn score_in_unit_interval() {
        let s = stats(1000, 10.0);
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), s.clone());
        let p = scorer.score(3, 10, 50);
        assert!(p > 0.0 && p < 1.0, "got {p}");
    }

    #[test]
    fn score_monotone_in_tf() {
        let s = stats(1000, 10.0);
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), s.clone());
        let mut last = scorer.score(0, 10, 50);
        for tf in 1..20 {
            let cur = scorer.score(tf, 10, 50);
            assert!(cur > last, "tf {tf}: {last} -> {cur}");
            last = cur;
        }
    }

    #[test]
    fn upper_bound_dominates_observed_scores() {
        let s = stats(1000, 10.0);
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), s.clone());
        let ub = scorer.upper_bound(50);
        for tf in [1, 5, 10, 100] {
            for dl in [1, 5, 50, 500] {
                let p = scorer.score(tf, dl, 50);
                assert!(p <= ub + 1e-12, "tf={tf} dl={dl}: {p} > {ub}");
            }
        }
    }

    #[test]
    fn query_level_calibration_uses_the_bm25_sum() {
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), stats(1000, 10.0));
        let combined = scorer.combine_scores(&[0.7, 0.4]);
        assert!((combined - sigmoid(1.1)).abs() < 1e-12);
    }

    #[test]
    fn query_level_calibration_preserves_raw_ranking() {
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), stats(1000, 10.0));
        let lower = scorer.combine_scores(&[0.7, 0.4]);
        let higher = scorer.combine_scores(&[0.8, 0.5]);
        assert!(higher > lower, "{higher} must exceed {lower}");
    }

    #[test]
    fn base_rate_is_added_once_at_query_level() {
        let scorer = BayesianBM25Scorer::new(
            BayesianBM25Params {
                base_rate: 0.1,
                ..BayesianBM25Params::default()
            },
            stats(1000, 10.0),
        );
        let combined = scorer.combine_scores(&[0.7, 0.4]);
        let expected = sigmoid(1.1 + logit(0.1));
        assert!((combined - expected).abs() < 1e-12);
    }
}
