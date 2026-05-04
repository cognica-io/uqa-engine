//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bayesian BM25 scorer (Section 4, Paper 3).

use uqa_core::IndexStats;

use crate::bayesian::BayesianProbabilityTransform;
use crate::bm25::{BM25Params, BM25Scorer};
use crate::prob::log_odds_conjunction;

#[derive(Debug, Clone, Copy)]
pub struct BayesianBM25Params {
    pub bm25: BM25Params,
    pub alpha: f64,
    pub beta: f64,
    /// Corpus base rate. `0.5` is treated as no base-rate correction
    /// (matches `None` in the Python API).
    pub base_rate: f64,
}

impl Default for BayesianBM25Params {
    fn default() -> Self {
        Self {
            bm25: BM25Params::default(),
            alpha: 1.0,
            beta: 0.0,
            base_rate: 0.5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BayesianBM25Scorer<'a> {
    pub params: BayesianBM25Params,
    pub bm25: BM25Scorer<'a>,
    transform: BayesianProbabilityTransform,
}

impl<'a> BayesianBM25Scorer<'a> {
    pub fn new(params: BayesianBM25Params, stats: &'a IndexStats) -> Self {
        let base_rate = if (params.base_rate - 0.5).abs() < f64::EPSILON {
            None
        } else {
            Some(params.base_rate)
        };
        let transform = BayesianProbabilityTransform::new(params.alpha, params.beta, base_rate);
        Self {
            params,
            bm25: BM25Scorer::new(params.bm25, stats),
            transform,
        }
    }

    pub fn idf(&self, doc_freq: u64) -> f64 {
        self.bm25.idf(doc_freq)
    }

    /// Full Bayesian BM25 posterior with three-term decomposition.
    pub fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64 {
        let idf_val = self.bm25.idf(doc_freq);
        self.score_with_idf(term_freq, doc_length, idf_val)
    }

    pub fn score_with_idf(&self, term_freq: u64, doc_length: u64, idf_val: f64) -> f64 {
        let raw = self.bm25.score_with_idf(term_freq, doc_length, idf_val);
        let avg_dl = self.bm25.stats.avg_doc_length;
        let doc_len_ratio = if avg_dl > 0.0 {
            doc_length as f64 / avg_dl
        } else {
            1.0
        };
        self.transform.score_to_probability(raw, term_freq as f64, doc_len_ratio)
    }

    /// Combine per-term Bayesian probabilities via log-odds conjunction
    /// (Paper 4 Section 4) with `alpha = 0` so the combined value is the
    /// plain mean log-odds put back through sigmoid (no `n^alpha`
    /// confidence amplification at this stage).
    pub fn combine_scores(scores: &[f64]) -> f64 {
        match scores.len() {
            0 => 0.5,
            1 => scores[0],
            _ => log_odds_conjunction(scores, 0.0),
        }
    }

    /// Bayesian WAND upper bound (Theorem 6.1.2): tightest safe pruning
    /// bound derived from the BM25 supremum and the maximum prior.
    pub fn upper_bound(&self, doc_freq: u64) -> f64 {
        let bm25_ub = self.bm25.upper_bound(doc_freq);
        self.transform.wand_upper_bound(bm25_ub, 0.9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(n: u64, avgdl: f64) -> IndexStats {
        let mut s = IndexStats::default();
        s.total_docs = n;
        s.avg_doc_length = avgdl;
        s
    }

    #[test]
    fn score_in_unit_interval() {
        let s = stats(1000, 10.0);
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), &s);
        let p = scorer.score(3, 10, 50);
        assert!(p > 0.0 && p < 1.0, "got {p}");
    }

    #[test]
    fn score_monotone_in_tf() {
        let s = stats(1000, 10.0);
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), &s);
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
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), &s);
        let ub = scorer.upper_bound(50);
        for tf in [1, 5, 10, 100] {
            for dl in [1, 5, 50, 500] {
                let p = scorer.score(tf, dl, 50);
                assert!(p <= ub + 1e-12, "tf={tf} dl={dl}: {p} > {ub}");
            }
        }
    }

    #[test]
    fn combine_scores_n1_is_identity() {
        let combined = BayesianBM25Scorer::combine_scores(&[0.7]);
        assert!((combined - 0.7).abs() < 1e-12);
    }

    #[test]
    fn combine_scores_idempotent_for_equal_inputs() {
        let combined = BayesianBM25Scorer::combine_scores(&[0.6, 0.6, 0.6]);
        assert!((combined - 0.6).abs() < 1e-9, "got {combined}");
    }
}
