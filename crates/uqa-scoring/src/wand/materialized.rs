//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialized posting-list cursor state and exact WAND/BMW pivot loops.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;

use uqa_core::{DocId, FieldName, Payload, PostingEntry, PostingList};
use uqa_storage::{BlockMaxIndex, InvertedIndex, StorageBackendResult};

use crate::scorer::Scorer;

use super::common::{
    invalid_wand_input, require_nonnegative_finite, update_top_k, HeapEntry, WANDResult, WANDStats,
    INF_DOC,
};

/// Common per-term cursor state: the entry slice and current position.
/// Field, term, and scorer live on [`WANDQuery`] so a single cursor
/// stays small and pivot reordering touches just one cache line.
struct TermCursor<'a> {
    entries: &'a [PostingEntry],
    position: usize,
    doc_freq: u64,
    upper_bound: f64,
}

impl<'a> TermCursor<'a> {
    fn current_doc(&self) -> u64 {
        self.entries
            .get(self.position)
            .map_or(INF_DOC, |e| e.doc_id)
    }

    fn current(&self) -> Option<&'a PostingEntry> {
        self.entries.get(self.position)
    }

    /// Binary search advance to the first entry with `doc_id >= target`.
    fn advance_to(&mut self, target: u64) {
        let mut lo = self.position;
        let mut hi = self.entries.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.entries[mid].doc_id < target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        self.position = lo;
    }
}

/// WAND configuration shared by both algorithms.
pub struct WANDQuery {
    pub posting_lists: Vec<PostingList>,
    pub scorers: Vec<Arc<dyn Scorer>>,
    pub fields: Vec<FieldName>,
    pub terms: Vec<String>,
    pub k: usize,
}

impl WANDQuery {
    pub fn new(
        posting_lists: Vec<PostingList>,
        scorers: Vec<Arc<dyn Scorer>>,
        fields: Vec<FieldName>,
        terms: Vec<String>,
        k: usize,
    ) -> StorageBackendResult<Self> {
        let expected = posting_lists.len();
        if scorers.len() != expected || fields.len() != expected || terms.len() != expected {
            return Err(invalid_wand_input(format!(
                "WAND term arrays must have equal lengths: posting_lists={expected}, scorers={}, fields={}, terms={}",
                scorers.len(),
                fields.len(),
                terms.len()
            )));
        }
        Ok(Self {
            posting_lists,
            scorers,
            fields,
            terms,
            k,
        })
    }
}

/// Standard WAND with per-term `term_upper_bound(df)` pruning.
pub struct WANDScorer<'a> {
    query: &'a WANDQuery,
    inverted_index: Option<&'a dyn InvertedIndex>,
}

impl<'a> WANDScorer<'a> {
    pub fn new(query: &'a WANDQuery, inverted_index: Option<&'a dyn InvertedIndex>) -> Self {
        Self {
            query,
            inverted_index,
        }
    }

    pub fn score_top_k(&self) -> StorageBackendResult<WANDResult> {
        validate_query(self.query)?;
        let mut cursors = build_cursors(self.query)?;
        run_pivot_loop(self.query, &mut cursors, self.inverted_index, |_, _, _| {
            Ok(false)
        })
    }
}

/// Block-Max WAND: pivot pruning uses per-block max scores from
/// [`BlockMaxIndex`] for tighter bounds.
pub struct BlockMaxWANDScorer<'a> {
    query: &'a WANDQuery,
    inverted_index: Option<&'a dyn InvertedIndex>,
    block_max_index: &'a BlockMaxIndex,
    table: String,
}

impl<'a> BlockMaxWANDScorer<'a> {
    pub fn new(
        query: &'a WANDQuery,
        inverted_index: Option<&'a dyn InvertedIndex>,
        block_max_index: &'a BlockMaxIndex,
        table: impl Into<String>,
    ) -> Self {
        Self {
            query,
            inverted_index,
            block_max_index,
            table: table.into(),
        }
    }

    pub fn score_top_k(&self) -> StorageBackendResult<WANDResult> {
        validate_query(self.query)?;
        let mut cursors = build_cursors(self.query)?;
        let q = self.query;
        let bmi = self.block_max_index;
        let table = &self.table;
        let suffix_bounds = q
            .fields
            .iter()
            .zip(&q.terms)
            .map(|(field, term)| {
                let Some(blocks) = bmi.block_maxes(table, field, term) else {
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
        run_pivot_loop(
            q,
            &mut cursors,
            self.inverted_index,
            |sorted_terms, cursors, bounds| {
                for &(doc_val, ti) in sorted_terms {
                    if doc_val == INF_DOC {
                        bounds.push(0.0);
                        continue;
                    }
                    let cur_block = bmi.block_index_for(cursors[ti].position)?;
                    // Take the max block-max across the remaining blocks
                    // for this term so the bound stays valid for any
                    // pivot doc the cursor could still reach. Anchoring
                    // on just `cur_block` under-counts a later block
                    // whose max score exceeds the current block, which
                    // would prune candidates BMW must still consider.
                    let bm = suffix_bounds[ti].get(cur_block).copied().unwrap_or(0.0);
                    // Fall back to the per-term `term_upper_bound(df)` if no
                    // block was recorded; an unindexed term must not get
                    // pruned more aggressively than plain WAND.
                    let bound = if bm > 0.0 {
                        bm
                    } else {
                        cursors[ti].upper_bound
                    };
                    bounds.push(bound);
                }
                Ok(true)
            },
        )
    }
}

fn build_cursors(query: &WANDQuery) -> StorageBackendResult<Vec<TermCursor<'_>>> {
    let mut cursors = Vec::with_capacity(query.posting_lists.len());
    for i in 0..query.posting_lists.len() {
        let entries = query.posting_lists[i].entries();
        let df = u64::try_from(entries.len())
            .map_err(|_| invalid_wand_input("posting-list length does not fit in u64"))?;
        let upper_bound = query.scorers[i].term_upper_bound(df);
        require_nonnegative_finite(upper_bound, "WAND term upper bound")?;
        cursors.push(TermCursor {
            entries,
            position: 0,
            doc_freq: df,
            upper_bound,
        });
    }
    Ok(cursors)
}

fn validate_query(query: &WANDQuery) -> StorageBackendResult<()> {
    let expected = query.posting_lists.len();
    if query.scorers.len() == expected
        && query.fields.len() == expected
        && query.terms.len() == expected
    {
        Ok(())
    } else {
        Err(invalid_wand_input(format!(
            "WAND term arrays must have equal lengths: posting_lists={expected}, scorers={}, fields={}, terms={}",
            query.scorers.len(),
            query.fields.len(),
            query.terms.len()
        )))
    }
}

fn build_field_slots(fields: &[FieldName]) -> (Vec<usize>, usize) {
    let mut unique_fields = Vec::<&str>::with_capacity(fields.len());
    let slots = fields
        .iter()
        .map(|field| {
            if let Some(slot) = unique_fields.iter().position(|known| *known == field) {
                slot
            } else {
                let slot = unique_fields.len();
                unique_fields.push(field);
                slot
            }
        })
        .collect();
    (slots, unique_fields.len())
}

/// Core pivot loop shared by WAND and BMW. `bound_provider` fills a reusable
/// per-term bound vector aligned with the current `sorted_terms` order and
/// returns `true`; `false` selects each cursor's precomputed upper bound.
fn run_pivot_loop<F>(
    query: &WANDQuery,
    cursors: &mut [TermCursor<'_>],
    inverted_index: Option<&dyn InvertedIndex>,
    mut bound_provider: F,
) -> StorageBackendResult<WANDResult>
where
    F: FnMut(&[(u64, usize)], &[TermCursor<'_>], &mut Vec<f64>) -> StorageBackendResult<bool>,
{
    let num_terms = query.posting_lists.len();
    let total_candidates = candidate_union(&query.posting_lists)?;
    if num_terms == 0 || query.k == 0 {
        return Ok(WANDResult {
            top_k: PostingList::new(),
            stats: WANDStats {
                total_candidates,
                ..WANDStats::default()
            },
        });
    }
    let candidate_capacity = usize::try_from(total_candidates).unwrap_or(usize::MAX);
    let mut top_k: BinaryHeap<HeapEntry> =
        BinaryHeap::with_capacity(query.k.min(candidate_capacity));
    let mut threshold = 0.0_f64;
    let mut stats = WANDStats {
        total_candidates,
        ..WANDStats::default()
    };

    let mut sorted_terms: Vec<(u64, usize)> = (0..num_terms)
        .map(|i| (cursors[i].current_doc(), i))
        .collect();
    sorted_terms.sort_unstable();
    let mut bounds = Vec::with_capacity(num_terms);
    let mut term_scores = Vec::with_capacity(num_terms);
    let (field_slots, field_count) = build_field_slots(&query.fields);
    let mut doc_lengths = vec![None; field_count];

    while !sorted_terms.is_empty() {
        if sorted_terms[0].0 == INF_DOC {
            break;
        }

        bounds.clear();
        if !bound_provider(&sorted_terms, cursors, &mut bounds)? {
            bounds.extend(sorted_terms.iter().map(|&(doc_val, ti)| {
                if doc_val == INF_DOC {
                    0.0
                } else {
                    cursors[ti].upper_bound
                }
            }));
        }
        let Some(pivot_idx) = select_pivot(query, &sorted_terms, &bounds, threshold)? else {
            break;
        };

        let pivot_doc = sorted_terms[pivot_idx].0;
        let first_doc = sorted_terms[0].0;

        if first_doc == pivot_doc {
            let actual_score = score_document(
                query,
                cursors,
                inverted_index,
                pivot_doc as DocId,
                &field_slots,
                &mut doc_lengths,
                &mut term_scores,
            )?;
            stats.scored = stats
                .scored
                .checked_add(1)
                .ok_or_else(|| invalid_wand_input("scored-document counter overflowed"))?;

            update_top_k(
                &mut top_k,
                query.k,
                actual_score,
                pivot_doc as DocId,
                &mut threshold,
            );

            // Advance every cursor at pivot_doc.
            for st in &mut sorted_terms {
                let ti = st.1;
                if cursors[ti].current_doc() == pivot_doc {
                    cursors[ti].position += 1;
                    st.0 = cursors[ti].current_doc();
                }
            }
            sorted_terms.sort_unstable();
        } else {
            // Skip first cursor forward to pivot_doc.
            let first_term = sorted_terms[0].1;
            cursors[first_term].advance_to(pivot_doc);
            stats.cursor_advances = stats
                .cursor_advances
                .checked_add(1)
                .ok_or_else(|| invalid_wand_input("cursor-advance counter overflowed"))?;
            sorted_terms[0].0 = cursors[first_term].current_doc();
            sorted_terms.sort_unstable();
        }
    }

    let mut entries: Vec<PostingEntry> = top_k
        .into_sorted_vec()
        .into_iter()
        .rev()
        .map(|h| PostingEntry::new(h.doc_id, Payload::with_score(h.score)))
        .collect();
    entries.sort_by_key(|e| e.doc_id);
    Ok(WANDResult {
        top_k: PostingList::from_sorted_unchecked(entries),
        stats,
    })
}

fn select_pivot(
    query: &WANDQuery,
    sorted_terms: &[(u64, usize)],
    bounds: &[f64],
    threshold: f64,
) -> StorageBackendResult<Option<usize>> {
    if bounds.len() != sorted_terms.len() {
        return Err(invalid_wand_input(format!(
            "bound provider returned {} bounds for {} terms",
            bounds.len(),
            sorted_terms.len()
        )));
    }
    for bound in bounds {
        require_nonnegative_finite(*bound, "WAND pruning bound")?;
    }
    for (index, &(doc_id, _)) in sorted_terms.iter().enumerate() {
        if doc_id == INF_DOC {
            break;
        }
        let cumulative = query.scorers[0].finalize_upper_bound(&bounds[..=index]);
        require_nonnegative_finite(cumulative, "WAND cumulative upper bound")?;
        if cumulative >= threshold {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

pub(super) fn candidate_union(posting_lists: &[PostingList]) -> StorageBackendResult<u64> {
    let mut positions = vec![0_usize; posting_lists.len()];
    let mut next = BinaryHeap::<Reverse<(DocId, usize)>>::with_capacity(posting_lists.len());
    for (list_index, posting) in posting_lists.iter().enumerate() {
        if let Some(entry) = posting.entries().first() {
            next.push(Reverse((entry.doc_id, list_index)));
        }
    }
    let mut count = 0_u64;
    let mut previous = None;
    while let Some(Reverse((doc_id, list_index))) = next.pop() {
        if previous != Some(doc_id) {
            count = count
                .checked_add(1)
                .ok_or_else(|| invalid_wand_input("candidate union length does not fit in u64"))?;
            previous = Some(doc_id);
        }
        let entries = posting_lists[list_index].entries();
        let position = &mut positions[list_index];
        while entries
            .get(*position)
            .is_some_and(|entry| entry.doc_id == doc_id)
        {
            *position += 1;
        }
        if let Some(entry) = entries.get(*position) {
            next.push(Reverse((entry.doc_id, list_index)));
        }
    }
    Ok(count)
}

/// Score a single document against every term cursor. Cursors that
/// point at a different `doc_id` contribute nothing; cursors that point
/// at `target` contribute a raw term score. The query score is finalized
/// once after all term contributions have been collected.
fn score_document(
    query: &WANDQuery,
    cursors: &[TermCursor<'_>],
    inverted_index: Option<&dyn InvertedIndex>,
    target: DocId,
    field_slots: &[usize],
    doc_lengths: &mut [Option<u64>],
    term_scores: &mut Vec<f64>,
) -> StorageBackendResult<f64> {
    doc_lengths.fill(None);
    term_scores.clear();
    for (i, cursor) in cursors.iter().enumerate() {
        let Some(entry) = cursor.current() else {
            continue;
        };
        if entry.doc_id != target {
            continue;
        }
        let tf = if entry.payload.positions.is_empty() {
            1
        } else {
            u64::try_from(entry.payload.positions.len())
                .map_err(|_| invalid_wand_input("term frequency does not fit in u64"))?
        };
        let df = cursor.doc_freq;
        let doc_length = match inverted_index {
            Some(idx) => {
                let slot = field_slots[i];
                let length = if let Some(length) = doc_lengths[slot] {
                    length
                } else {
                    let length = idx.get_doc_length(target, &query.fields[i])?;
                    doc_lengths[slot] = Some(length);
                    length
                };
                length.max(tf)
            }
            None => tf,
        };
        let term_score = query.scorers[i].term_score(tf, doc_length, df);
        require_nonnegative_finite(term_score, "WAND term score")?;
        term_scores.push(term_score);
    }
    let score = query.scorers[0].finalize_score(term_scores);
    require_nonnegative_finite(score, "WAND finalized score")?;
    Ok(score)
}
