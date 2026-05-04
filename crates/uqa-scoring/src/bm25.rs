//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standard BM25 scorer (Definition 3.2.1, Paper 3).
//!
//! Properties (Theorem 3.2.2):
//! - monotonically increasing in term frequency,
//! - monotonically decreasing in document length,
//! - upper bound `boost * IDF` (Theorem 3.2.3).

use std::sync::Arc;

use uqa_core::IndexStats;
use uqa_storage::BlockMaxScorer;

#[derive(Debug, Clone, Copy)]
pub struct BM25Params {
    pub k1: f64,
    pub b: f64,
    pub boost: f64,
}

impl Default for BM25Params {
    fn default() -> Self {
        Self {
            k1: 1.2,
            b: 0.75,
            boost: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BM25Scorer {
    pub params: BM25Params,
    pub stats: Arc<IndexStats>,
}

impl BM25Scorer {
    pub fn new(params: BM25Params, stats: Arc<IndexStats>) -> Self {
        Self { params, stats }
    }

    /// Robertson-Sparck-Jones IDF (Definition 3.1.1, Paper 3):
    /// `ln((N - df + 0.5) / (df + 0.5) + 1)`.
    pub fn idf(&self, doc_freq: u64) -> f64 {
        let n = self.stats.total_docs as f64;
        let df = doc_freq as f64;
        ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
    }

    /// Numerically stable BM25 score:
    ///
    /// `score(f, n) = w - w / (1 + f * inv_norm)`,
    /// where `w = boost * IDF` and
    /// `inv_norm = 1 / (k1 * ((1-b) + b * dl/avgdl))`.
    pub fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64 {
        let idf_val = self.idf(doc_freq);
        self.score_with_idf(term_freq, doc_length, idf_val)
    }

    pub fn score_with_idf(&self, term_freq: u64, doc_length: u64, idf_val: f64) -> f64 {
        let w = self.params.boost * idf_val;
        let avg_dl = if self.stats.avg_doc_length > 0.0 {
            self.stats.avg_doc_length
        } else {
            1.0
        };
        let b_factor = (1.0 - self.params.b) + self.params.b * (doc_length as f64 / avg_dl);
        let inv_norm = 1.0 / (self.params.k1 * b_factor);
        w - w / (1.0 + term_freq as f64 * inv_norm)
    }

    /// Theorem 3.2.3 supremum: `boost * IDF(df)`.
    pub fn upper_bound(&self, doc_freq: u64) -> f64 {
        self.params.boost * self.idf(doc_freq)
    }

    /// BM25 scores combine additively across query terms.
    pub fn combine_scores(scores: &[f64]) -> f64 {
        scores.iter().sum()
    }
}

impl BlockMaxScorer for BM25Scorer {
    fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64 {
        BM25Scorer::score(self, term_freq, doc_length, doc_freq)
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
    fn idf_rises_as_df_falls() {
        let s = stats(1000, 10.0);
        let bm = BM25Scorer::new(BM25Params::default(), s.clone());
        let high_df = bm.idf(900);
        let low_df = bm.idf(10);
        assert!(low_df > high_df);
        assert!(high_df >= 0.0);
    }

    #[test]
    fn score_strictly_increases_in_tf() {
        let s = stats(1000, 10.0);
        let bm = BM25Scorer::new(BM25Params::default(), s.clone());
        let mut last = bm.score(0, 10, 50);
        for tf in 1..30 {
            let cur = bm.score(tf, 10, 50);
            assert!(cur > last, "score must rise with tf: {last} -> {cur}");
            last = cur;
        }
    }

    #[test]
    fn score_strictly_decreases_in_dl_for_fixed_tf() {
        let s = stats(1000, 10.0);
        let bm = BM25Scorer::new(BM25Params::default(), s.clone());
        let mut last = bm.score(5, 1, 50);
        for dl in 2..30 {
            let cur = bm.score(5, dl, 50);
            assert!(cur < last, "score must fall with dl: {last} -> {cur}");
            last = cur;
        }
    }

    #[test]
    fn supremum_is_boost_times_idf() {
        let s = stats(1000, 10.0);
        let bm = BM25Scorer::new(BM25Params::default(), s.clone());
        let bound = bm.upper_bound(50);
        // For very large tf the score approaches w = boost * IDF.
        let very_large = bm.score(1_000_000, 10, 50);
        assert!(very_large < bound + 1e-9);
        assert!(very_large > 0.999 * bound);
    }
}
