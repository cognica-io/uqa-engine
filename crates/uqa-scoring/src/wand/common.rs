//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared top-k heap, result statistics, and bound validation.

use std::cmp::{Ordering, Reverse};
use std::collections::{binary_heap::PeekMut, BinaryHeap};
use std::sync::Arc;

use crate::Scorer;
use uqa_core::{DocId, FieldName, Payload, PostingEntry, PostingList, ScoredEntry};
use uqa_storage::{BlockMaxIndex, StorageBackendError, StorageBackendResult, TokenTermKey};

/// Min-heap entry by score for top-k selection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeapEntry {
    pub(crate) score: f64,
    pub(crate) doc_id: DocId,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // `BinaryHeap` is a max-heap; flip the score comparison so the
        // root holds the *minimum* score (the eviction candidate). On a
        // score tie, the entry with the *larger* doc id sits at the
        // root, matching the conventional "lower doc id wins" tie break
        // applied at output time.
        match other.score.total_cmp(&self.score) {
            Ordering::Equal => self.doc_id.cmp(&other.doc_id),
            ord => ord,
        }
    }
}

pub(crate) fn update_top_k(
    top_k: &mut BinaryHeap<HeapEntry>,
    k: usize,
    score: f64,
    doc_id: DocId,
    threshold: &mut f64,
) {
    let candidate = HeapEntry { score, doc_id };
    if top_k.len() < k {
        top_k.push(candidate);
        if top_k.len() == k {
            *threshold = top_k.peek().map_or(0.0, |entry| entry.score);
        }
        return;
    }
    let Some(mut eviction) = top_k.peek_mut() else {
        return;
    };
    if score > eviction.score || (score == eviction.score && doc_id < eviction.doc_id) {
        *eviction = candidate;
        drop(eviction);
        *threshold = top_k.peek().map_or(*threshold, |entry| entry.score);
    }
}

/// Count the support union without retaining candidate identities. Each input
/// yields its document identifiers in ascending order.
pub(crate) fn count_distinct_docs<I: Iterator<Item = DocId>>(
    inputs: impl IntoIterator<Item = I>,
) -> Option<usize> {
    let mut inputs: Vec<_> = inputs.into_iter().collect();
    let mut heads: BinaryHeap<_> = inputs
        .iter_mut()
        .enumerate()
        .filter_map(|(index, input)| input.next().map(|doc| Reverse((doc, index))))
        .collect();
    let mut count = 0usize;
    let mut previous = None;
    while let Some(mut head) = heads.peek_mut() {
        let Reverse((doc, index)) = *head;
        if previous != Some(doc) {
            count = count.checked_add(1)?;
            previous = Some(doc);
        }
        if let Some(next) = inputs[index].next() {
            *head = Reverse((next, index));
        } else {
            PeekMut::pop(head);
        }
    }
    Some(count)
}

/// Stats collected during a top-k pass; tests use these to assert the
/// exit-criterion skip rates from the master plan.
///
/// `total_candidates` is the sum of term document frequencies, an upper bound
/// on the number of distinct candidates. Materialized queries can request
/// the exact union explicitly with [`super::WANDQuery::exact_candidate_count`].
/// `scored` counts full document evaluations and `cursor_advances` counts
/// pivot-driven seeks.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct WANDStats {
    pub scored: u64,
    pub total_candidates: u64,
    pub cursor_advances: u64,
}

impl WANDStats {
    pub fn skip_rate(&self) -> f64 {
        if self.total_candidates == 0 {
            0.0
        } else {
            1.0 - (self.scored as f64 / self.total_candidates as f64)
        }
    }
}

#[derive(Debug, Clone)]
pub struct WANDResult {
    pub top_k: PostingList,
    pub stats: WANDStats,
}

pub(super) fn invalid_wand_input(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid WAND input: {}", message.into()))
}

/// Storage cursors carry term frequency and document length. Positional
/// lists obtain them from the index and share one length lookup per field.
pub(super) trait WandCursorSource {
    const NAME: &'static str;
    const COLLECTION: &'static str;

    fn current_doc(&self) -> Option<DocId>;
    fn ordinal(&self) -> StorageBackendResult<usize>;
    fn advance(&mut self) -> StorageBackendResult<()>;
    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<()>;
    fn term_inputs(
        &self,
        doc: DocId,
        lengths: &mut [Option<u64>],
    ) -> StorageBackendResult<Option<(u64, u64)>>;
}

pub(super) fn validate_shape<C: WandCursorSource>(
    count: usize,
    scorers: usize,
    fields: usize,
    terms: usize,
) -> StorageBackendResult<()> {
    if count == scorers && count == fields && count == terms {
        return Ok(());
    }
    Err(invalid_wand_input(format!(
        "{} term arrays must have equal lengths: {}={count}, scorers={scorers}, fields={fields}, terms={terms}",
        C::NAME, C::COLLECTION
    )))
}

fn check_score<C: WandCursorSource>(value: f64, name: &str) -> StorageBackendResult<()> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(invalid_wand_input(format!(
            "{} {name} must be finite and non-negative, got {value}",
            C::NAME
        )))
    }
}

pub(super) struct WandTerm<C> {
    source: C,
    doc_freq: u64,
    idf: f64,
    upper_bound: f64,
}

impl<C: WandCursorSource> WandTerm<C> {
    pub(super) fn new(source: C, doc_freq: u64, scorer: &dyn Scorer) -> StorageBackendResult<Self> {
        let (idf, upper_bound) = scorer.wand_term_constants(doc_freq);
        check_score::<C>(upper_bound, "term upper bound")?;
        Ok(Self {
            source,
            doc_freq,
            idf,
            upper_bound,
        })
    }
}

pub(super) struct BlockBounds<'a> {
    index: &'a BlockMaxIndex,
    suffixes: Vec<Vec<f64>>,
}

impl<'a> BlockBounds<'a> {
    pub(super) fn new(
        index: &'a BlockMaxIndex,
        table: &str,
        fields: &[FieldName],
        terms: &[TokenTermKey],
    ) -> Self {
        let suffixes = fields
            .iter()
            .zip(terms)
            .map(|(field, term)| {
                let mut suffix = index
                    .block_maxes_key(table, field, term)
                    .unwrap_or_default()
                    .to_vec();
                let mut maximum = 0.0_f64;
                for bound in suffix.iter_mut().rev() {
                    maximum = maximum.max(*bound);
                    *bound = maximum;
                }
                suffix
            })
            .collect();
        Self { index, suffixes }
    }

    fn bound<C: WandCursorSource>(
        &self,
        term: usize,
        state: &WandTerm<C>,
    ) -> StorageBackendResult<f64> {
        let block = self.index.block_index_for(state.source.ordinal()?)?;
        // A later block can exceed the current block. Missing or zero block
        // metadata uses the ordinary term bound.
        Ok(self.suffixes[term]
            .get(block)
            .copied()
            .filter(|bound| *bound > 0.0)
            .unwrap_or(state.upper_bound))
    }
}

pub(super) struct WandSelection {
    heap: BinaryHeap<HeapEntry>,
    stats: WANDStats,
}

impl WandSelection {
    pub(super) fn into_postings(self) -> WANDResult {
        let mut entries: Vec<_> = self
            .heap
            .into_iter()
            .map(|entry| PostingEntry::new(entry.doc_id, Payload::with_score(entry.score)))
            .collect();
        entries.sort_unstable_by_key(|entry| entry.doc_id);
        WANDResult {
            top_k: PostingList::from_sorted_unchecked(entries),
            stats: self.stats,
        }
    }

    pub(super) fn into_ranked(self) -> (Vec<ScoredEntry>, WANDStats) {
        let ranked = self
            .heap
            .into_sorted_vec()
            .into_iter()
            .map(|entry| ScoredEntry {
                doc_id: entry.doc_id,
                score: entry.score,
            })
            .collect();
        (ranked, self.stats)
    }
}

pub(super) fn run_pivot_loop<C: WandCursorSource>(
    terms: &mut [WandTerm<C>],
    scorers: &[Arc<dyn Scorer>],
    k: usize,
    field_count: usize,
    block_bounds: Option<&BlockBounds<'_>>,
) -> StorageBackendResult<WandSelection> {
    let total_candidates = terms.iter().try_fold(0u64, |total, term| {
        total
            .checked_add(term.doc_freq)
            .ok_or_else(|| invalid_wand_input("cursor candidate count overflowed"))
    })?;
    let mut stats = WANDStats {
        total_candidates,
        ..WANDStats::default()
    };
    let largest = terms.iter().map(|term| term.doc_freq).max().unwrap_or(0);
    let capacity = usize::try_from(largest).unwrap_or(usize::MAX).min(k);
    let mut heap = BinaryHeap::with_capacity(capacity);
    if k == 0 || terms.is_empty() {
        return Ok(WandSelection { heap, stats });
    }
    let mut order: Vec<_> = terms
        .iter()
        .enumerate()
        .filter_map(|(i, term)| term.source.current_doc().map(|doc| (doc, i)))
        .collect();
    order.sort_unstable();
    let mut bounds = Vec::with_capacity(terms.len());
    let mut contributions = Vec::with_capacity(terms.len());
    let mut lengths = vec![None; field_count];
    let mut threshold = 0.0;
    while let Some(&(first_doc, first_term)) = order.first() {
        bounds.clear();
        for &(_, i) in &order {
            let bound = match block_bounds {
                Some(blocks) => blocks.bound(i, &terms[i])?,
                None => terms[i].upper_bound,
            };
            bounds.push(bound);
        }
        let Some(pivot) = select_pivot::<C>(scorers[0].as_ref(), &bounds, threshold)? else {
            break;
        };
        let doc = order[pivot].0;
        if doc == first_doc {
            lengths.fill(None);
            contributions.clear();
            // The scalar sum follows query order independently of pivot ordering.
            for (term, scorer) in terms.iter().zip(scorers) {
                if let Some((frequency, length)) = term.source.term_inputs(doc, &mut lengths)? {
                    let score = scorer.term_score_with_idf(frequency, length, term.idf);
                    check_score::<C>(score, "term score")?;
                    contributions.push(score);
                }
            }
            let score = scorers[0].finalize_score(&contributions);
            check_score::<C>(score, "finalized score")?;
            stats.scored = stats
                .scored
                .checked_add(1)
                .ok_or_else(|| invalid_wand_input("scored-document counter overflowed"))?;
            update_top_k(&mut heap, k, score, doc, &mut threshold);
            for &(_, i) in order.iter().take_while(|(id, _)| *id == doc) {
                terms[i].source.advance()?;
            }
        } else {
            terms[first_term].source.advance_to(doc)?;
            stats.cursor_advances = stats
                .cursor_advances
                .checked_add(1)
                .ok_or_else(|| invalid_wand_input("cursor-advance counter overflowed"))?;
        }
        order.retain_mut(|(doc, i)| {
            if let Some(current) = terms[*i].source.current_doc() {
                *doc = current;
                true
            } else {
                false
            }
        });
        order.sort_unstable();
    }
    Ok(WandSelection { heap, stats })
}

fn select_pivot<C: WandCursorSource>(
    scorer: &dyn Scorer,
    bounds: &[f64],
    threshold: f64,
) -> StorageBackendResult<Option<usize>> {
    for &bound in bounds {
        check_score::<C>(bound, "pruning bound")?;
    }
    let mut sum = std::iter::empty::<f64>().sum::<f64>();
    for (i, &bound) in bounds.iter().enumerate() {
        sum += bound;
        let cumulative = scorer.finalize_upper_bound_with_sum(&bounds[..=i], sum);
        check_score::<C>(cumulative, "cumulative upper bound")?;
        if cumulative >= threshold {
            return Ok(Some(i));
        }
    }
    Ok(None)
}
