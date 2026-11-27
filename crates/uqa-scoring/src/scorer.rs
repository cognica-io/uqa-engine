//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-scoring contract shared by BM25 and Bayesian BM25.
//!
//! Term contributions stay on the raw BM25 scale until every query term
//! has been combined. A scorer then finalizes the query score. Plain BM25
//! sums the contributions; Bayesian BM25 applies one monotone calibration
//! to that sum.

use crate::bayesian_bm25::BayesianBM25Scorer;
use crate::bm25::BM25Scorer;

pub trait Scorer: Send + Sync {
    fn idf(&self, doc_freq: u64) -> f64;
    fn term_score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64;
    fn term_score_with_idf(&self, term_freq: u64, doc_length: u64, idf_value: f64) -> f64;
    fn finalize_score(&self, term_scores: &[f64]) -> f64;
    fn term_upper_bound(&self, doc_freq: u64) -> f64;

    fn finalize_upper_bound(&self, term_upper_bounds: &[f64]) -> f64 {
        self.finalize_score(term_upper_bounds)
    }

    /// IDF for document scoring and the bound for pruning this term.
    fn wand_term_constants(&self, doc_freq: u64) -> (f64, f64) {
        (self.idf(doc_freq), self.term_upper_bound(doc_freq))
    }

    /// The supplied sum follows the order of `term_upper_bounds`. Scorers
    /// with a non-additive bound retain their full-prefix implementation.
    fn finalize_upper_bound_with_sum(&self, term_upper_bounds: &[f64], _sum: f64) -> f64 {
        self.finalize_upper_bound(term_upper_bounds)
    }
}

impl Scorer for BM25Scorer {
    fn idf(&self, doc_freq: u64) -> f64 {
        BM25Scorer::idf(self, doc_freq)
    }

    fn term_score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64 {
        BM25Scorer::score(self, term_freq, doc_length, doc_freq)
    }

    fn term_score_with_idf(&self, term_freq: u64, doc_length: u64, idf_value: f64) -> f64 {
        BM25Scorer::score_with_idf(self, term_freq, doc_length, idf_value)
    }

    fn finalize_score(&self, term_scores: &[f64]) -> f64 {
        BM25Scorer::combine_scores(term_scores)
    }

    fn term_upper_bound(&self, doc_freq: u64) -> f64 {
        BM25Scorer::upper_bound(self, doc_freq)
    }

    fn wand_term_constants(&self, doc_freq: u64) -> (f64, f64) {
        BM25Scorer::wand_term_constants(self, doc_freq)
    }

    fn finalize_upper_bound_with_sum(&self, _term_upper_bounds: &[f64], sum: f64) -> f64 {
        sum
    }
}

impl Scorer for BayesianBM25Scorer {
    fn idf(&self, doc_freq: u64) -> f64 {
        BayesianBM25Scorer::idf(self, doc_freq)
    }

    fn term_score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64 {
        self.bm25.score(term_freq, doc_length, doc_freq)
    }

    fn term_score_with_idf(&self, term_freq: u64, doc_length: u64, idf_value: f64) -> f64 {
        self.bm25.score_with_idf(term_freq, doc_length, idf_value)
    }

    fn finalize_score(&self, term_scores: &[f64]) -> f64 {
        self.calibrate_raw_value(BM25Scorer::combine_scores(term_scores))
    }

    fn term_upper_bound(&self, doc_freq: u64) -> f64 {
        self.bm25.upper_bound(doc_freq)
    }

    fn wand_term_constants(&self, doc_freq: u64) -> (f64, f64) {
        self.bm25.wand_term_constants(doc_freq)
    }

    fn finalize_upper_bound_with_sum(&self, _term_upper_bounds: &[f64], sum: f64) -> f64 {
        self.calibrate_raw_value(sum)
    }
}
