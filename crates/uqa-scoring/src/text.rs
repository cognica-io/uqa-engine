//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical lexical scoring independent of Engine sessions, catalogs, and SQL.

use crate::{rank_scored_entries_top_k, BM25Scorer, ScoringError, ScoringMode};
use exhaustive::{score_multiple_text_terms, score_single_text_term};
use statistics::{block_max_scorer_fingerprint, raw_bm25_params, search_stats_for_terms};
use std::{sync::Arc, time::Instant};
use uqa_core::ScoredEntry;
use uqa_storage::{
    inverted_index::analyze_query_terms, InvertedIndex, StorageBackendError, TokenTermKey,
};

mod candidates;
mod exhaustive;
mod statistics;
mod top_k;

pub use candidates::TextCandidateScorer;

/// Algorithm that actually produced a text-search result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSearchAlgorithm {
    Exhaustive,
    Wand,
    BlockMaxWand,
}

/// Observable work counters for one text top-k execution.
#[derive(Debug, Clone)]
pub struct TextSearchProfile {
    pub entries: Vec<ScoredEntry>,
    pub algorithm: TextSearchAlgorithm,
    pub scored_candidates: u64,
    /// Exact distinct candidates for exhaustive/materialized execution; for
    /// score-cursor WAND/BMW this is the sum of term document frequencies, a
    /// no-prescan upper bound on the distinct candidate count.
    pub total_candidates: u64,
    pub cursor_advances: u64,
    pub skip_rate: f64,
    pub elapsed_ms: f64,
}

/// Preserve parameter errors separately from backend and index-integrity failures.
#[derive(Debug, thiserror::Error)]
pub enum TextSearchError {
    #[error("{0}")]
    Parameters(#[from] ScoringError),
    #[error("{action}: {source}")]
    Storage {
        action: &'static str,
        #[source]
        source: StorageBackendError,
    },
    #[error("{0}")]
    InvalidIndex(String),
}

fn storage_error(action: &'static str, error: impl Into<StorageBackendError>) -> TextSearchError {
    TextSearchError::Storage {
        action,
        source: error.into(),
    }
}

/// Analyze the complete query with the retained field revision and execute its physical scoring strategy.
pub fn score_text_query(
    index: &dyn InvertedIndex,
    table: &str,
    field: &str,
    query: &str,
    mode: &ScoringMode,
    top_k: usize,
    strategy: TextSearchAlgorithm,
) -> Result<TextSearchProfile, TextSearchError> {
    let started = Instant::now();
    let analyzer = index
        .search_analyzer_revision(field)
        .map_err(|error| storage_error("resolve text analyzer revision", error))?;
    let terms = analyze_query_terms(&analyzer, query)
        .map_err(|error| storage_error("analyze text query", error))?;
    let mut profile = score_text_terms(index, table, field, &terms, mode, top_k, strategy)?;
    profile.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(profile)
}

/// Score ordered lossless term occurrences; repeated query terms remain independent scoring contributions.
pub fn score_text_terms(
    index: &dyn InvertedIndex,
    table: &str,
    field: &str,
    analyzed_terms: &[TokenTermKey],
    mode: &ScoringMode,
    top_k: usize,
    strategy: TextSearchAlgorithm,
) -> Result<TextSearchProfile, TextSearchError> {
    let started = Instant::now();
    if !analyzed_terms.is_empty() && strategy != TextSearchAlgorithm::Exhaustive {
        let (entries, stats, algorithm) =
            top_k::score_text_top_k(index, table, field, analyzed_terms, mode, top_k, strategy)?;
        return Ok(TextSearchProfile {
            entries,
            algorithm,
            scored_candidates: stats.scored,
            total_candidates: stats.total_candidates,
            cursor_advances: stats.cursor_advances,
            skip_rate: stats.skip_rate(),
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        });
    }
    let entries = match analyzed_terms.len() {
        0 => Vec::new(),
        1 => score_single_text_term(index, field, analyzed_terms, mode)?,
        _ => score_multiple_text_terms(index, field, analyzed_terms, mode)?,
    };
    let total_candidates = u64::try_from(entries.len())
        .map_err(|_| TextSearchError::InvalidIndex("text candidate count exceeds u64".into()))?;
    Ok(TextSearchProfile {
        entries: rank_scored_entries_top_k(entries, top_k),
        algorithm: TextSearchAlgorithm::Exhaustive,
        scored_candidates: total_candidates,
        total_candidates,
        cursor_advances: 0,
        skip_rate: 0.0,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

/// Materialize block bounds with the exact corpus statistics and scorer identity used during query execution.
pub fn rebuild_text_block_max(
    index: &mut dyn InvertedIndex,
    field: &str,
    mode: &ScoringMode,
) -> Result<bool, TextSearchError> {
    let stats = Arc::new(
        index
            .field_stats_scalar(field)
            .map_err(|error| storage_error("read field statistics", error))?,
    );
    let params = raw_bm25_params(mode);
    params.validate()?;
    let fingerprint = block_max_scorer_fingerprint(params, stats.as_ref());
    let scorer = BM25Scorer::new(params, stats);
    index
        .rebuild_persisted_block_max(field, &scorer, &fingerprint)
        .map_err(|error| storage_error("rebuild persisted block-max scores", error))
}
