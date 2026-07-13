//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! WAND and Block-Max WAND top-k scorers (Section 6, Paper 3).
//!
//! Both implementations advance posting-list cursors through pivot
//! resolution. Pruning is *exact* under their respective upper-bound
//! contracts: for WAND the per-term `term_upper_bound(df)`; for BMW the
//! tighter per-block max stored in [`BlockMaxIndex`]. The output top-k
//! is identical to exhaustive scoring.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

use uqa_core::{DocId, FieldName, Payload, PostingEntry, PostingList};
use uqa_storage::{BlockMaxIndex, InvertedIndex};

use crate::scorer::Scorer;

const INF_DOC: u64 = u64::MAX;

/// Min-heap entry by score for top-k selection.
#[derive(Debug, Clone, Copy)]
struct HeapEntry {
    score: f64,
    doc_id: DocId,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score && self.doc_id == other.doc_id
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
        match other
            .score
            .partial_cmp(&self.score)
            .unwrap_or(Ordering::Equal)
        {
            Ordering::Equal => self.doc_id.cmp(&other.doc_id),
            ord => ord,
        }
    }
}

/// Common per-term cursor state: the entry slice and current position.
/// Field, term, and scorer live on [`WANDQuery`] so a single cursor
/// stays small and pivot reordering touches just one cache line.
struct TermCursor<'a> {
    entries: &'a [PostingEntry],
    position: usize,
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
    ) -> Self {
        debug_assert_eq!(posting_lists.len(), scorers.len());
        debug_assert_eq!(posting_lists.len(), fields.len());
        debug_assert_eq!(posting_lists.len(), terms.len());
        Self {
            posting_lists,
            scorers,
            fields,
            terms,
            k,
        }
    }
}

/// Stats collected during a top-k pass; tests use these to assert the
/// exit-criterion skip rates from the master plan.
///
/// Skip rate semantics: `1 - scored / total_candidates`, where
/// `total_candidates` is the size of the union of all input posting
/// lists (every distinct doc id any term cursor could have visited).
/// `scored` counts how many of those candidates we actually evaluated
/// the full BM25 sum for; `cursor_advances` counts pivot-driven binary
/// search steps and is informational only.
#[derive(Debug, Default, Clone, Copy)]
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

    pub fn score_top_k(&self) -> WANDResult {
        let mut cursors = build_cursors(self.query);
        run_pivot_loop(self.query, &mut cursors, self.inverted_index, |_, _| None)
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

    pub fn score_top_k(&self) -> WANDResult {
        let mut cursors = build_cursors(self.query);
        let q = self.query;
        let bmi = self.block_max_index;
        let table = &self.table;
        run_pivot_loop(
            q,
            &mut cursors,
            self.inverted_index,
            |sorted_terms, cursors| {
                let mut bounds: Vec<f64> = Vec::with_capacity(sorted_terms.len());
                for &(doc_val, ti) in sorted_terms {
                    if doc_val == INF_DOC {
                        bounds.push(0.0);
                        continue;
                    }
                    let cur_block = bmi.block_index_for(cursors[ti].position);
                    let total_blocks = bmi.num_blocks(table, &q.fields[ti], &q.terms[ti]);
                    // Take the max block-max across the remaining blocks
                    // for this term so the bound stays valid for any
                    // pivot doc the cursor could still reach. Anchoring
                    // on just `cur_block` under-counts a later block
                    // whose max score exceeds the current block, which
                    // would prune candidates BMW must still consider.
                    let mut bm = 0.0_f64;
                    for b in cur_block..total_blocks {
                        let v = bmi.block_max(table, &q.fields[ti], &q.terms[ti], b);
                        if v > bm {
                            bm = v;
                        }
                    }
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
                Some(bounds)
            },
        )
    }
}

fn build_cursors(query: &WANDQuery) -> Vec<TermCursor<'_>> {
    let mut cursors = Vec::with_capacity(query.posting_lists.len());
    for i in 0..query.posting_lists.len() {
        let entries = query.posting_lists[i].entries();
        let df = entries.len() as u64;
        let upper_bound = query.scorers[i].term_upper_bound(df);
        cursors.push(TermCursor {
            entries,
            position: 0,
            upper_bound,
        });
    }
    cursors
}

/// Core pivot loop shared by WAND and BMW. `bound_provider` returns a
/// per-term bound vector aligned with the *current* `sorted_terms`
/// order; `None` means "use each cursor's pre-computed `upper_bound`"
/// (plain WAND). BMW returns `Some` with the per-block bounds.
fn run_pivot_loop<F>(
    query: &WANDQuery,
    cursors: &mut [TermCursor<'_>],
    inverted_index: Option<&dyn InvertedIndex>,
    mut bound_provider: F,
) -> WANDResult
where
    F: FnMut(&[(u64, usize)], &[TermCursor<'_>]) -> Option<Vec<f64>>,
{
    let num_terms = query.posting_lists.len();
    if num_terms == 0 {
        return WANDResult {
            top_k: PostingList::new(),
            stats: WANDStats::default(),
        };
    }

    let mut top_k: BinaryHeap<HeapEntry> = BinaryHeap::with_capacity(query.k);
    let mut threshold = 0.0_f64;
    let mut stats = WANDStats {
        total_candidates: candidate_union(&query.posting_lists),
        ..WANDStats::default()
    };

    let mut sorted_terms: Vec<(u64, usize)> = (0..num_terms)
        .map(|i| (cursors[i].current_doc(), i))
        .collect();
    sorted_terms.sort_unstable();

    while !sorted_terms.is_empty() {
        if sorted_terms[0].0 == INF_DOC {
            break;
        }

        let bounds = bound_provider(&sorted_terms, cursors).unwrap_or_else(|| {
            sorted_terms
                .iter()
                .map(|&(doc_val, ti)| {
                    if doc_val == INF_DOC {
                        0.0
                    } else {
                        cursors[ti].upper_bound
                    }
                })
                .collect()
        });

        let mut cumulative_bounds = Vec::with_capacity(bounds.len());
        let mut pivot: Option<usize> = None;
        for (idx, &(doc_val, _)) in sorted_terms.iter().enumerate() {
            if doc_val == INF_DOC {
                break;
            }
            cumulative_bounds.push(bounds[idx]);
            let cumulative = query.scorers[0].finalize_upper_bound(&cumulative_bounds);
            if cumulative >= threshold {
                pivot = Some(idx);
                break;
            }
        }

        let Some(pivot_idx) = pivot else {
            break;
        };

        let pivot_doc = sorted_terms[pivot_idx].0;
        let first_doc = sorted_terms[0].0;

        if first_doc == pivot_doc {
            let actual_score = score_document(query, cursors, inverted_index, pivot_doc as DocId);
            stats.scored += 1;

            if top_k.len() < query.k {
                top_k.push(HeapEntry {
                    score: actual_score,
                    doc_id: pivot_doc as DocId,
                });
                if top_k.len() == query.k {
                    threshold = top_k.peek().map_or(0.0, |e| e.score);
                }
            } else if actual_score > threshold {
                top_k.pop();
                top_k.push(HeapEntry {
                    score: actual_score,
                    doc_id: pivot_doc as DocId,
                });
                threshold = top_k.peek().map_or(threshold, |e| e.score);
            }

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
            stats.cursor_advances += 1;
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
    WANDResult {
        top_k: PostingList::from_sorted_unchecked(entries),
        stats,
    }
}

fn candidate_union(posting_lists: &[PostingList]) -> u64 {
    let mut ids: std::collections::BTreeSet<DocId> = std::collections::BTreeSet::default();
    for pl in posting_lists {
        for entry in pl {
            ids.insert(entry.doc_id);
        }
    }
    ids.len() as u64
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
) -> f64 {
    let mut term_scores = Vec::with_capacity(cursors.len());
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
            entry.payload.positions.len() as u64
        };
        let df = query.posting_lists[i].len() as u64;
        let doc_length = match inverted_index {
            Some(idx) => idx.get_doc_length(target, &query.fields[i]).max(tf),
            None => tf,
        };
        term_scores.push(query.scorers[i].term_score(tf, doc_length, df));
    }
    query.scorers[0].finalize_score(&term_scores)
}

/// Track upper-bound tightness: ratio of `actual_max / upper_bound` per
/// posting list, averaged across all observations.
#[derive(Debug, Default, Clone)]
pub struct BoundTightnessAnalyzer {
    pairs: Vec<(f64, f64)>,
}

impl BoundTightnessAnalyzer {
    pub fn record(&mut self, upper_bound: f64, actual_max: f64) {
        self.pairs.push((upper_bound, actual_max));
    }

    pub fn tightness_ratio(&self) -> f64 {
        if self.pairs.is_empty() {
            return 1.0;
        }
        let n = self.pairs.len() as f64;
        let s: f64 = self
            .pairs
            .iter()
            .map(|&(ub, am)| if ub > 0.0 { (am / ub).min(1.0) } else { 1.0 })
            .sum();
        s / n
    }

    pub fn slack(&self) -> f64 {
        1.0 - self.tightness_ratio()
    }

    pub fn worst_bound_index(&self) -> usize {
        self.pairs
            .iter()
            .enumerate()
            .min_by(|(_, (ub_a, actual_a)), (_, (ub_b, actual_b))| {
                let ratio_a = if *ub_a > 0.0 {
                    (*actual_a / *ub_a).min(1.0)
                } else {
                    1.0
                };
                let ratio_b = if *ub_b > 0.0 {
                    (*actual_b / *ub_b).min(1.0)
                } else {
                    1.0
                };
                ratio_a
                    .partial_cmp(&ratio_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map_or(0, |(idx, _)| idx)
    }

    pub fn clear(&mut self) {
        self.pairs.clear();
    }
}

pub struct AdaptiveWANDScorer {
    pub scorers: Vec<Arc<dyn Scorer>>,
    pub k: usize,
    pub posting_lists: Vec<PostingList>,
    pub tightening_factor: f64,
    pub analyzer: BoundTightnessAnalyzer,
}

impl AdaptiveWANDScorer {
    pub fn new(
        scorers: Vec<Arc<dyn Scorer>>,
        k: usize,
        posting_lists: Vec<PostingList>,
        tightening_factor: f64,
    ) -> Self {
        Self {
            scorers,
            k,
            posting_lists,
            tightening_factor,
            analyzer: BoundTightnessAnalyzer::default(),
        }
    }

    pub fn compute_upper_bounds(&self) -> Vec<f64> {
        self.scorers
            .iter()
            .zip(&self.posting_lists)
            .map(|(scorer, pl)| scorer.term_upper_bound(pl.len() as u64) * self.tightening_factor)
            .collect()
    }

    pub fn score_top_k(&mut self) -> PostingList {
        self.analyzer.clear();
        for (scorer, pl) in self.scorers.iter().zip(&self.posting_lists) {
            let upper = scorer.term_upper_bound(pl.len() as u64);
            let actual = pl.iter().map(|e| e.payload.score).fold(0.0_f64, f64::max);
            self.analyzer.record(upper, actual);
        }

        let mut scores: std::collections::BTreeMap<DocId, f64> = std::collections::BTreeMap::new();
        for pl in &self.posting_lists {
            for entry in pl {
                *scores.entry(entry.doc_id).or_insert(0.0) += entry.payload.score;
            }
        }
        let mut entries: Vec<PostingEntry> = scores
            .into_iter()
            .map(|(doc_id, score)| PostingEntry::new(doc_id, Payload::with_score(score)))
            .collect();
        entries.sort_by(|a, b| {
            b.payload
                .score
                .partial_cmp(&a.payload.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        entries.truncate(self.k);
        PostingList::from_unsorted(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::IndexStats;

    use crate::bayesian_bm25::{BayesianBM25Params, BayesianBM25Scorer};
    use crate::bm25::{BM25Params, BM25Scorer};

    fn pl_from_tfs(tfs: &[(DocId, u64)]) -> PostingList {
        let entries: Vec<PostingEntry> = tfs
            .iter()
            .map(|&(doc_id, tf)| {
                let positions: Vec<u32> = (0..tf as u32).collect();
                PostingEntry::new(
                    doc_id,
                    Payload {
                        positions,
                        score: 0.0,
                        fields: std::collections::BTreeMap::default(),
                    },
                )
            })
            .collect();
        PostingList::from_unsorted(entries)
    }

    fn bm25(stats: Arc<IndexStats>) -> Arc<dyn Scorer> {
        Arc::new(BM25Scorer::new(BM25Params::default(), stats))
    }

    #[test]
    fn wand_top_k_matches_exhaustive_scoring() {
        let mut stats = IndexStats::default();
        stats.total_docs = 10;
        stats.avg_doc_length = 5.0;
        let stats = Arc::new(stats);

        let pl_rust = pl_from_tfs(&[(1, 3), (2, 1), (4, 2), (5, 5), (8, 1)]);
        let pl_lang = pl_from_tfs(&[(1, 1), (3, 4), (4, 1), (6, 2), (8, 3)]);
        let scorers = vec![bm25(stats.clone()), bm25(stats.clone())];

        let q = WANDQuery::new(
            vec![pl_rust.clone(), pl_lang.clone()],
            scorers.clone(),
            vec!["title".into(), "title".into()],
            vec!["rust".into(), "lang".into()],
            3,
        );
        let wand = WANDScorer::new(&q, None);
        let result = wand.score_top_k();

        // Exhaustive baseline: score every doc that appears in either
        // list and sort.
        let mut expected: Vec<(DocId, f64)> = Vec::new();
        let mut seen: std::collections::BTreeSet<DocId> = std::collections::BTreeSet::default();
        for pl in [&pl_rust, &pl_lang] {
            for entry in pl {
                seen.insert(entry.doc_id);
            }
        }
        for &doc_id in &seen {
            let mut term_scores = Vec::new();
            for (pl, scorer) in [&pl_rust, &pl_lang].iter().zip(scorers.iter()) {
                if let Some(e) = pl.get_entry(doc_id) {
                    let tf = if e.payload.positions.is_empty() {
                        1
                    } else {
                        e.payload.positions.len() as u64
                    };
                    term_scores.push(scorer.term_score(tf, tf, pl.len() as u64));
                }
            }
            let s = scorers[0].finalize_score(&term_scores);
            expected.push((doc_id, s));
        }
        expected.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        expected.truncate(3);

        let mut got: Vec<(DocId, f64)> = result
            .top_k
            .iter()
            .map(|e| (e.doc_id, e.payload.score))
            .collect();
        got.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        assert_eq!(got.len(), expected.len());
        for ((d1, s1), (d2, s2)) in got.iter().zip(&expected) {
            assert_eq!(d1, d2);
            assert!((s1 - s2).abs() < 1e-9, "{s1} vs {s2}");
        }
    }

    #[test]
    fn bayesian_wand_finalizes_the_complete_query_once() {
        let mut stats = IndexStats::default();
        stats.total_docs = 10;
        stats.avg_doc_length = 5.0;
        let stats = Arc::new(stats);
        let params = BayesianBM25Params {
            alpha: 1.4,
            beta: 0.7,
            base_rate: 0.1,
            ..BayesianBM25Params::default()
        };
        let posting_lists = vec![
            pl_from_tfs(&[(1, 3), (2, 1), (4, 2), (5, 5), (8, 1)]),
            pl_from_tfs(&[(1, 1), (3, 4), (4, 1), (6, 2), (8, 3)]),
        ];
        let scorers: Vec<Arc<dyn Scorer>> = (0..2)
            .map(|_| Arc::new(BayesianBM25Scorer::new(params, stats.clone())) as Arc<dyn Scorer>)
            .collect();
        let query = WANDQuery::new(
            posting_lists.clone(),
            scorers.clone(),
            vec!["title".into(), "title".into()],
            vec!["rust".into(), "language".into()],
            3,
        );
        let result = WANDScorer::new(&query, None).score_top_k();

        let mut candidate_ids = std::collections::BTreeSet::new();
        for posting_list in &posting_lists {
            candidate_ids.extend(posting_list.iter().map(|entry| entry.doc_id));
        }
        let mut expected = Vec::new();
        for doc_id in candidate_ids {
            let mut term_scores = Vec::new();
            for (posting_list, scorer) in posting_lists.iter().zip(&scorers) {
                if let Some(entry) = posting_list.get_entry(doc_id) {
                    let term_frequency = entry.payload.positions.len() as u64;
                    term_scores.push(scorer.term_score(
                        term_frequency,
                        term_frequency,
                        posting_list.len() as u64,
                    ));
                }
            }
            expected.push((doc_id, scorers[0].finalize_score(&term_scores)));
        }
        expected.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.0.cmp(&right.0))
        });
        expected.truncate(3);

        let mut actual: Vec<(DocId, f64)> = result
            .top_k
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score))
            .collect();
        actual.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.0.cmp(&right.0))
        });
        assert_eq!(actual.len(), expected.len());
        for ((actual_doc, actual_score), (expected_doc, expected_score)) in
            actual.iter().zip(&expected)
        {
            assert_eq!(actual_doc, expected_doc);
            assert!((actual_score - expected_score).abs() < 1e-12);
        }
    }

    #[test]
    fn bound_tightness_default_is_one() {
        let a = BoundTightnessAnalyzer::default();
        assert!((a.tightness_ratio() - 1.0).abs() < 1e-12);
        assert!((a.slack() - 0.0).abs() < 1e-12);
    }

    #[test]
    fn bound_tightness_records_ratio() {
        let mut a = BoundTightnessAnalyzer::default();
        a.record(1.0, 0.8);
        a.record(2.0, 1.0);
        // ratios: 0.8, 0.5; mean 0.65
        assert!((a.tightness_ratio() - 0.65).abs() < 1e-9);
    }
}
