//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Score-only posting cursor state and exact WAND/BMW pivot loops.

use std::collections::BinaryHeap;
use std::sync::Arc;

use uqa_core::{DocId, FieldName, Payload, PostingEntry, PostingList};
use uqa_storage::{BlockMaxIndex, PostingCursor, StorageBackendResult};

use crate::scorer::Scorer;

use super::common::{
    invalid_wand_input, require_nonnegative_finite, update_top_k, WANDResult, WANDStats, INF_DOC,
};

/// WAND query backed directly by score-only posting cursors.
///
/// Unlike [`super::materialized::WANDQuery`], this form never materializes positional payloads and
/// carries document length beside term frequency in each cursor entry.
pub struct CursorWANDQuery {
    pub cursors: Vec<Box<dyn PostingCursor>>,
    pub scorers: Vec<Arc<dyn Scorer>>,
    pub fields: Vec<FieldName>,
    pub terms: Vec<String>,
    pub k: usize,
}

impl CursorWANDQuery {
    pub fn new(
        cursors: Vec<Box<dyn PostingCursor>>,
        scorers: Vec<Arc<dyn Scorer>>,
        fields: Vec<FieldName>,
        terms: Vec<String>,
        k: usize,
    ) -> StorageBackendResult<Self> {
        let expected = cursors.len();
        if scorers.len() != expected || fields.len() != expected || terms.len() != expected {
            return Err(invalid_wand_input(format!(
                "cursor WAND term arrays must have equal lengths: cursors={expected}, scorers={}, fields={}, terms={}",
                scorers.len(),
                fields.len(),
                terms.len()
            )));
        }
        Ok(Self {
            cursors,
            scorers,
            fields,
            terms,
            k,
        })
    }
}

struct ScoreTermCursor {
    cursor: Box<dyn PostingCursor>,
    upper_bound: f64,
}

impl ScoreTermCursor {
    fn current_doc(&self) -> DocId {
        self.cursor.current().map_or(INF_DOC, |entry| entry.doc_id)
    }

    fn block_ordinal(&self) -> StorageBackendResult<usize> {
        usize::try_from(self.cursor.ordinal())
            .map_err(|_| invalid_wand_input("posting cursor ordinal does not fit in usize"))
    }
}

/// Standard WAND over score-only posting cursors.
pub struct CursorWANDScorer<'a> {
    query: &'a CursorWANDQuery,
}

impl<'a> CursorWANDScorer<'a> {
    pub fn new(query: &'a CursorWANDQuery) -> Self {
        Self { query }
    }

    pub fn score_top_k(&self) -> StorageBackendResult<WANDResult> {
        validate_cursor_query(self.query)?;
        let mut cursors = build_score_cursors(self.query)?;
        run_cursor_pivot_loop(self.query, &mut cursors, |_, _, _| Ok(false))
    }
}

/// Block-Max WAND over score-only posting cursors.
pub struct CursorBlockMaxWANDScorer<'a> {
    query: &'a CursorWANDQuery,
    block_max_index: &'a BlockMaxIndex,
    table: String,
}

impl<'a> CursorBlockMaxWANDScorer<'a> {
    pub fn new(
        query: &'a CursorWANDQuery,
        block_max_index: &'a BlockMaxIndex,
        table: impl Into<String>,
    ) -> Self {
        Self {
            query,
            block_max_index,
            table: table.into(),
        }
    }

    pub fn score_top_k(&self) -> StorageBackendResult<WANDResult> {
        validate_cursor_query(self.query)?;
        let mut cursors = build_score_cursors(self.query)?;
        let query = self.query;
        let block_max = self.block_max_index;
        let suffix_bounds = query
            .fields
            .iter()
            .zip(&query.terms)
            .map(|(field, term)| {
                let Some(blocks) = block_max.block_maxes(&self.table, field, term) else {
                    return Vec::new();
                };
                let mut suffix = vec![0.0_f64; blocks.len()];
                let mut maximum = 0.0_f64;
                for (index, score) in blocks.iter().enumerate().rev() {
                    maximum = maximum.max(*score);
                    suffix[index] = maximum;
                }
                suffix
            })
            .collect::<Vec<_>>();
        run_cursor_pivot_loop(query, &mut cursors, |sorted_terms, cursors, bounds| {
            for &(doc_id, term_index) in sorted_terms {
                if doc_id == INF_DOC {
                    bounds.push(0.0);
                    continue;
                }
                let block_index =
                    block_max.block_index_for(cursors[term_index].block_ordinal()?)?;
                let block_bound = suffix_bounds[term_index]
                    .get(block_index)
                    .copied()
                    .unwrap_or(0.0);
                bounds.push(if block_bound > 0.0 {
                    block_bound
                } else {
                    cursors[term_index].upper_bound
                });
            }
            Ok(true)
        })
    }
}

fn validate_cursor_query(query: &CursorWANDQuery) -> StorageBackendResult<()> {
    let expected = query.cursors.len();
    if query.scorers.len() == expected
        && query.fields.len() == expected
        && query.terms.len() == expected
    {
        Ok(())
    } else {
        Err(invalid_wand_input(format!(
            "cursor WAND term arrays must have equal lengths: cursors={expected}, scorers={}, fields={}, terms={}",
            query.scorers.len(),
            query.fields.len(),
            query.terms.len()
        )))
    }
}

fn build_score_cursors(query: &CursorWANDQuery) -> StorageBackendResult<Vec<ScoreTermCursor>> {
    query
        .cursors
        .iter()
        .cloned()
        .zip(&query.scorers)
        .map(|(cursor, scorer)| {
            let upper_bound = scorer.term_upper_bound(cursor.doc_freq());
            require_nonnegative_finite(upper_bound, "cursor WAND term upper bound")?;
            Ok(ScoreTermCursor {
                cursor,
                upper_bound,
            })
        })
        .collect()
}

fn cursor_candidate_upper_bound(query: &CursorWANDQuery) -> StorageBackendResult<u64> {
    query.cursors.iter().try_fold(0_u64, |total, cursor| {
        total
            .checked_add(cursor.doc_freq())
            .ok_or_else(|| invalid_wand_input("cursor candidate count overflowed"))
    })
}

fn run_cursor_pivot_loop<F>(
    query: &CursorWANDQuery,
    cursors: &mut [ScoreTermCursor],
    mut bound_provider: F,
) -> StorageBackendResult<WANDResult>
where
    F: FnMut(&[(DocId, usize)], &[ScoreTermCursor], &mut Vec<f64>) -> StorageBackendResult<bool>,
{
    let total_candidates = cursor_candidate_upper_bound(query)?;
    if cursors.is_empty() || query.k == 0 {
        return Ok(WANDResult {
            top_k: PostingList::new(),
            stats: WANDStats {
                total_candidates,
                ..WANDStats::default()
            },
        });
    }
    let candidate_capacity = usize::try_from(total_candidates).unwrap_or(usize::MAX);
    let mut top_k = BinaryHeap::with_capacity(query.k.min(candidate_capacity));
    let mut threshold = 0.0_f64;
    let mut stats = WANDStats {
        total_candidates,
        ..WANDStats::default()
    };
    let mut sorted_terms = cursors
        .iter()
        .enumerate()
        .map(|(index, cursor)| (cursor.current_doc(), index))
        .collect::<Vec<_>>();
    sorted_terms.sort_unstable();
    let mut bounds = Vec::with_capacity(cursors.len());
    let mut term_scores = Vec::with_capacity(cursors.len());

    while sorted_terms
        .first()
        .is_some_and(|(doc_id, _)| *doc_id != INF_DOC)
    {
        bounds.clear();
        if !bound_provider(&sorted_terms, cursors, &mut bounds)? {
            bounds.extend(sorted_terms.iter().map(|&(doc_id, term_index)| {
                if doc_id == INF_DOC {
                    0.0
                } else {
                    cursors[term_index].upper_bound
                }
            }));
        }
        let Some(pivot_index) = select_cursor_pivot(query, &sorted_terms, &bounds, threshold)?
        else {
            break;
        };
        let pivot_doc = sorted_terms[pivot_index].0;
        if sorted_terms[0].0 == pivot_doc {
            let score = score_cursor_document(query, cursors, pivot_doc, &mut term_scores)?;
            stats.scored = stats
                .scored
                .checked_add(1)
                .ok_or_else(|| invalid_wand_input("scored-document counter overflowed"))?;
            update_top_k(&mut top_k, query.k, score, pivot_doc, &mut threshold);
            for sorted in &mut sorted_terms {
                let term_index = sorted.1;
                if cursors[term_index].current_doc() == pivot_doc {
                    cursors[term_index].cursor.advance()?;
                    sorted.0 = cursors[term_index].current_doc();
                }
            }
            sorted_terms.sort_unstable();
        } else {
            let term_index = sorted_terms[0].1;
            cursors[term_index].cursor.advance_to(pivot_doc)?;
            stats.cursor_advances = stats
                .cursor_advances
                .checked_add(1)
                .ok_or_else(|| invalid_wand_input("cursor-advance counter overflowed"))?;
            sorted_terms[0].0 = cursors[term_index].current_doc();
            sorted_terms.sort_unstable();
        }
    }

    let mut entries = top_k
        .into_sorted_vec()
        .into_iter()
        .rev()
        .map(|entry| PostingEntry::new(entry.doc_id, Payload::with_score(entry.score)))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.doc_id);
    Ok(WANDResult {
        top_k: PostingList::from_sorted_unchecked(entries),
        stats,
    })
}

fn select_cursor_pivot(
    query: &CursorWANDQuery,
    sorted_terms: &[(DocId, usize)],
    bounds: &[f64],
    threshold: f64,
) -> StorageBackendResult<Option<usize>> {
    if bounds.len() != sorted_terms.len() {
        return Err(invalid_wand_input(format!(
            "cursor bound provider returned {} bounds for {} terms",
            bounds.len(),
            sorted_terms.len()
        )));
    }
    for bound in bounds {
        require_nonnegative_finite(*bound, "cursor WAND pruning bound")?;
    }
    for (index, &(doc_id, _)) in sorted_terms.iter().enumerate() {
        if doc_id == INF_DOC {
            break;
        }
        let cumulative = query.scorers[0].finalize_upper_bound(&bounds[..=index]);
        require_nonnegative_finite(cumulative, "cursor WAND cumulative upper bound")?;
        if cumulative >= threshold {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn score_cursor_document(
    query: &CursorWANDQuery,
    cursors: &[ScoreTermCursor],
    target: DocId,
    term_scores: &mut Vec<f64>,
) -> StorageBackendResult<f64> {
    term_scores.clear();
    for (index, cursor) in cursors.iter().enumerate() {
        let Some(entry) = cursor.cursor.current() else {
            continue;
        };
        if entry.doc_id != target {
            continue;
        }
        let term_score = query.scorers[index].term_score(
            entry.term_freq,
            entry.doc_length.max(entry.term_freq),
            cursor.cursor.doc_freq(),
        );
        require_nonnegative_finite(term_score, "cursor WAND term score")?;
        term_scores.push(term_score);
    }
    let score = query.scorers[0].finalize_score(term_scores);
    require_nonnegative_finite(score, "cursor WAND finalized score")?;
    Ok(score)
}
