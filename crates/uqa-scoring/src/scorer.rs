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
        self.calibrate_raw_score(BM25Scorer::combine_scores(term_scores))
    }

    fn term_upper_bound(&self, doc_freq: u64) -> f64 {
        self.bm25.upper_bound(doc_freq)
    }
}
