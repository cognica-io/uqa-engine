//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cursor WAND and persisted block-max selection.

use super::exhaustive::build_text_scorer;
use super::statistics::{block_max_scorer_fingerprint, raw_bm25_params, search_stats_for_terms};
use super::{storage_error, TextSearchAlgorithm, TextSearchError};
use crate::{
    rank_scored_entries_top_k, CursorBlockMaxWANDScorer, CursorWANDQuery, CursorWANDScorer,
    ScoringMode, WANDStats,
};
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::ScoredEntry;
use uqa_storage::{BlockMaxIndex, InvertedIndex, TokenTermKey, DEFAULT_BLOCK_SIZE};

pub(super) fn load_block_max_index(
    index: &dyn InvertedIndex,
    table: &str,
    field: &str,
    analyzed_terms: &[TokenTermKey],
    doc_freqs: &[u64],
    fingerprint: &str,
) -> Result<Option<BlockMaxIndex>, TextSearchError> {
    let mut block_max = BlockMaxIndex::new(DEFAULT_BLOCK_SIZE)
        .map_err(|error| storage_error("create block-max index", error))?;
    let mut checked = BTreeSet::new();
    let mut requested = Vec::<(TokenTermKey, usize)>::new();
    for (term, doc_freq) in analyzed_terms.iter().zip(doc_freqs) {
        if *doc_freq == 0 || !checked.insert(term) {
            continue;
        }
        let expected_blocks = usize::try_from(*doc_freq)
            .map_err(|_| {
                TextSearchError::InvalidIndex("text document frequency exceeds usize".into())
            })?
            .div_ceil(DEFAULT_BLOCK_SIZE);
        requested.push((term.clone(), expected_blocks));
    }
    let terms = requested
        .iter()
        .map(|(term, _)| term.clone())
        .collect::<Vec<_>>();
    let persisted = index
        .persisted_block_max_scores_keys_bulk(field, &terms, fingerprint)
        .map_err(|error| storage_error("read persisted block-max scores", error))?;
    if persisted.len() != requested.len() {
        return Err(TextSearchError::InvalidIndex(format!(
            "block-max bulk read returned {} terms for {} requests",
            persisted.len(),
            requested.len()
        )));
    }
    for ((term, expected_blocks), scores) in requested.into_iter().zip(persisted) {
        let Some(scores) = scores else {
            return Ok(None);
        };
        if scores.len() != expected_blocks {
            return Ok(None);
        }
        block_max
            .set_block_maxes_key(table, field, &term, scores)
            .map_err(|error| storage_error("load block-max scores", error))?;
    }
    Ok(Some(block_max))
}

pub(super) fn score_text_top_k(
    index: &dyn InvertedIndex,
    table: &str,
    field: &str,
    analyzed_terms: &[TokenTermKey],
    mode: &ScoringMode,
    top_k: usize,
    strategy: TextSearchAlgorithm,
) -> Result<(Vec<ScoredEntry>, WANDStats, TextSearchAlgorithm), TextSearchError> {
    let posting_cursors = index
        .posting_cursors_keys_bulk(field, analyzed_terms)
        .map_err(|error| storage_error("open text posting cursors", error))?;
    let doc_freqs = posting_cursors
        .iter()
        .map(|cursor| cursor.doc_freq())
        .collect::<Vec<_>>();
    let stats = Arc::new(
        search_stats_for_terms(index, field, analyzed_terms, &doc_freqs)
            .map_err(|error| storage_error("read field statistics", error))?,
    );
    let scorer = build_text_scorer(mode, stats.clone(), analyzed_terms.len())?;
    let wand_query = CursorWANDQuery::new_keys(
        posting_cursors,
        vec![scorer; analyzed_terms.len()],
        vec![field.to_string(); analyzed_terms.len()],
        analyzed_terms.to_vec(),
        top_k,
    )
    .map_err(|error| storage_error("build WAND query", error))?;

    let (result, algorithm) = match strategy {
        TextSearchAlgorithm::Exhaustive | TextSearchAlgorithm::Wand => (
            CursorWANDScorer::new(&wand_query)
                .score_top_k()
                .map_err(|error| storage_error("execute WAND", error))?,
            TextSearchAlgorithm::Wand,
        ),
        TextSearchAlgorithm::BlockMaxWand => {
            let fingerprint = block_max_scorer_fingerprint(raw_bm25_params(mode), stats.as_ref());
            if let Some(block_max) = load_block_max_index(
                index,
                table,
                field,
                analyzed_terms,
                &doc_freqs,
                &fingerprint,
            )? {
                (
                    CursorBlockMaxWANDScorer::new(&wand_query, &block_max, table)
                        .score_top_k()
                        .map_err(|error| storage_error("execute Block-Max WAND", error))?,
                    TextSearchAlgorithm::BlockMaxWand,
                )
            } else {
                // A concurrent or transactional posting mutation can
                // invalidate blocks after planning. Exact WAND is the safe
                // physical fallback; stale bounds are never consumed.
                (
                    CursorWANDScorer::new(&wand_query)
                        .score_top_k()
                        .map_err(|error| storage_error("execute WAND fallback", error))?,
                    TextSearchAlgorithm::Wand,
                )
            }
        }
    };
    let entries = rank_scored_entries_top_k(
        result.top_k.iter().map(ScoredEntry::from_entry).collect(),
        top_k,
    );
    Ok((entries, result.stats, algorithm))
}
