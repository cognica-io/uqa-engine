//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `Scorer` trait, the shape every relevance scorer (BM25, Bayesian BM25,
//! external priors, ...) implements so the operator layer can dispatch
//! through a single `&dyn Scorer`.

use crate::bayesian_bm25::BayesianBM25Scorer;
use crate::bm25::BM25Scorer;

pub trait Scorer: Send + Sync {
    fn idf(&self, doc_freq: u64) -> f64;
    fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64;
    fn score_with_idf(&self, term_freq: u64, doc_length: u64, idf_val: f64) -> f64;
    fn combine_scores(&self, scores: &[f64]) -> f64;
    fn upper_bound(&self, doc_freq: u64) -> f64;
}

impl Scorer for BM25Scorer {
    fn idf(&self, doc_freq: u64) -> f64 {
        BM25Scorer::idf(self, doc_freq)
    }

    fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64 {
        BM25Scorer::score(self, term_freq, doc_length, doc_freq)
    }

    fn score_with_idf(&self, term_freq: u64, doc_length: u64, idf_val: f64) -> f64 {
        BM25Scorer::score_with_idf(self, term_freq, doc_length, idf_val)
    }

    fn combine_scores(&self, scores: &[f64]) -> f64 {
        BM25Scorer::combine_scores(scores)
    }

    fn upper_bound(&self, doc_freq: u64) -> f64 {
        BM25Scorer::upper_bound(self, doc_freq)
    }
}

impl Scorer for BayesianBM25Scorer {
    fn idf(&self, doc_freq: u64) -> f64 {
        BayesianBM25Scorer::idf(self, doc_freq)
    }

    fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64 {
        BayesianBM25Scorer::score(self, term_freq, doc_length, doc_freq)
    }

    fn score_with_idf(&self, term_freq: u64, doc_length: u64, idf_val: f64) -> f64 {
        BayesianBM25Scorer::score_with_idf(self, term_freq, doc_length, idf_val)
    }

    fn combine_scores(&self, scores: &[f64]) -> f64 {
        BayesianBM25Scorer::combine_scores(scores)
    }

    fn upper_bound(&self, doc_freq: u64) -> f64 {
        BayesianBM25Scorer::upper_bound(self, doc_freq)
    }
}
