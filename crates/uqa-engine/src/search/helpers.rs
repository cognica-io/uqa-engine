//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared storage errors, BM25 statistics, and block-max fingerprints.

use super::{
    BM25Params, BTreeSet, IndexStats, InvertedIndex, SQLError, ScoringMode, StorageBackendError,
    StorageBackendResult, DEFAULT_BLOCK_SIZE,
};

pub(super) fn search_stats_for_terms(
    index: &dyn InvertedIndex,
    field: &str,
    terms: &[String],
    doc_freqs: &[u64],
) -> StorageBackendResult<IndexStats> {
    // The scalar variant skips the vocabulary-wide doc-freq map; every
    // term this query scores gets its frequency set explicitly below.
    let mut stats = index.field_stats_scalar(field)?;

    let mut seen = BTreeSet::<&str>::new();
    for (term, doc_freq) in terms.iter().zip(doc_freqs) {
        if seen.insert(term.as_str()) {
            stats.set_doc_freq(field.to_string(), term.clone(), *doc_freq);
        }
    }
    Ok(stats)
}

pub(super) fn raw_bm25_params(mode: &ScoringMode) -> BM25Params {
    match mode {
        ScoringMode::BM25(params) => *params,
        ScoringMode::BayesianBM25(params) => params.bm25,
    }
}

/// Stable identity for every value used while materializing raw BM25 block
/// bounds. Term document frequency is already reflected in each stored score;
/// posting mutations atomically invalidate the materialization.
pub(super) fn block_max_scorer_fingerprint(params: BM25Params, stats: &IndexStats) -> String {
    format!(
        "bm25-block-v1:block={DEFAULT_BLOCK_SIZE}:k1={:016x}:b={:016x}:boost={:016x}:docs={}:avgdl={:016x}",
        params.k1.to_bits(),
        params.b.to_bits(),
        params.boost.to_bits(),
        stats.total_docs,
        stats.avg_doc_length.to_bits(),
    )
}

pub(super) fn storage_sql_error(action: &str, error: impl Into<StorageBackendError>) -> SQLError {
    let error = error.into();
    SQLError::Internal(format!("{action}: {error}"))
}
