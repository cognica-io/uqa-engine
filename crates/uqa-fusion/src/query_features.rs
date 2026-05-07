//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-level features for attention-based fusion (Section 8, Paper 4).
//!
//! Six features in this fixed order:
//!   `[mean_idf, max_idf, min_idf, coverage_ratio, query_length,
//!     vocab_overlap_ratio]`.

use uqa_core::IndexStats;

pub const N_QUERY_FEATURES: usize = 6;

#[derive(Debug, Clone)]
pub struct QueryFeatureExtractor {
    stats: IndexStats,
    field: Option<String>,
}

impl QueryFeatureExtractor {
    pub fn new(stats: IndexStats) -> Self {
        Self { stats, field: None }
    }

    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    pub fn n_features(&self) -> usize {
        N_QUERY_FEATURES
    }

    pub fn extract<S: AsRef<str>>(&self, query_terms: &[S]) -> [f64; N_QUERY_FEATURES] {
        let terms: Vec<String> = query_terms
            .iter()
            .map(|term| term.as_ref().to_string())
            .collect();
        extract_query_features(&self.stats, &terms, self.field.as_deref())
    }
}

/// Compute the query feature vector against the supplied
/// [`IndexStats`]. `field` is the IDF field to consult; pass `None`
/// for the catch-all `_default` slot.
pub fn extract_query_features(
    stats: &IndexStats,
    query_terms: &[String],
    field: Option<&str>,
) -> [f64; N_QUERY_FEATURES] {
    let n_docs = stats.total_docs;
    if n_docs == 0 {
        return [0.0; N_QUERY_FEATURES];
    }
    let field_name = field.unwrap_or("_default");
    let mut idfs: Vec<f64> = Vec::with_capacity(query_terms.len());
    let mut vocab_hits: usize = 0;
    for term in query_terms {
        let df = stats.doc_freq(field_name, term);
        if df > 0 {
            vocab_hits += 1;
            let idf = (((n_docs - df) as f64 + 0.5) / (df as f64 + 0.5) + 1.0).ln();
            idfs.push(idf);
        }
    }
    if idfs.is_empty() {
        return [0.0, 0.0, 0.0, 0.0, query_terms.len() as f64, 0.0];
    }
    let mean_idf: f64 = idfs.iter().sum::<f64>() / idfs.len() as f64;
    let max_idf: f64 = idfs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let min_idf: f64 = idfs.iter().copied().fold(f64::INFINITY, f64::min);
    let coverage_ratio = idfs.len() as f64 / n_docs.max(1) as f64;
    let query_length = query_terms.len() as f64;
    let vocab_overlap = vocab_hits as f64 / query_terms.len().max(1) as f64;
    [
        mean_idf,
        max_idf,
        min_idf,
        coverage_ratio,
        query_length,
        vocab_overlap,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats_with(field: &str, terms: &[(&str, u64)]) -> IndexStats {
        let mut s = IndexStats::default();
        s.total_docs = 1000;
        s.avg_doc_length = 100.0;
        for (term, df) in terms {
            s.set_doc_freq(field, *term, *df);
        }
        s
    }

    #[test]
    fn empty_index_returns_zeros() {
        let stats = IndexStats::default();
        let feats = extract_query_features(&stats, &["foo".into()], None);
        assert_eq!(feats, [0.0; N_QUERY_FEATURES]);
    }

    #[test]
    fn unknown_terms_yield_idf_zeros_with_query_length() {
        let stats = stats_with("body", &[]);
        let feats = extract_query_features(&stats, &["unseen".into()], Some("body"));
        // mean/max/min/coverage = 0, query length = 1, vocab overlap = 0.
        assert_eq!(feats[0], 0.0);
        assert_eq!(feats[4], 1.0);
        assert_eq!(feats[5], 0.0);
    }

    #[test]
    fn known_terms_produce_positive_idf() {
        let stats = stats_with("body", &[("rust", 5), ("python", 200)]);
        let feats = extract_query_features(&stats, &["rust".into(), "python".into()], Some("body"));
        assert!(feats[0] > 0.0);
        assert!(feats[1] >= feats[0]);
        assert!(feats[2] <= feats[0]);
        assert!((feats[5] - 1.0).abs() < 1e-9);
    }
}
