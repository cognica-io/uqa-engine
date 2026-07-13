//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! WAND / Block-Max-WAND exactness (Master plan Section 2.3,
//! Theorem 4 from the master plan).
//!
//! Property: for any random corpus and any random subset of query
//! terms, both `WANDScorer::score_top_k` and
//! `BlockMaxWANDScorer::score_top_k` must return the same top-k doc-id
//! set as exhaustive scoring (i.e. compute every doc's BM25 sum and
//! sort). Pruning may not change correctness, only speed.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use proptest::prelude::*;
use uqa_core::{DocId, IndexStats, Payload, PostingEntry, PostingList};
use uqa_scoring::{
    bm25::BM25Scorer, BM25Params, BlockMaxWANDScorer, Scorer, WANDQuery, WANDScorer,
};
use uqa_storage::BlockMaxIndex;

const TABLE: &str = "docs";
const FIELD: &str = "body";

/// Per-term posting list: `(doc_id, tf)` pairs (sorted by `doc_id`,
/// no duplicates).
type TermPostings = Vec<(DocId, u64)>;

/// Builds a `PostingList` whose entries carry `tf` as the position
/// count, matching the convention `BlockMaxIndex::build` uses.
fn pl_from_tfs(tfs: &TermPostings) -> PostingList {
    let entries: Vec<PostingEntry> = tfs
        .iter()
        .map(|&(doc_id, tf)| {
            let positions: Vec<u32> = (0..tf as u32).collect();
            PostingEntry::new(
                doc_id,
                Payload {
                    positions,
                    score: 0.0,
                    fields: BTreeMap::default(),
                },
            )
        })
        .collect();
    PostingList::from_unsorted(entries)
}

/// Exhaustive baseline: score every doc that any term covers, sort by
/// score desc with `doc_id` asc as the tiebreaker, truncate to k.
fn exhaustive_top_k(
    posting_lists: &[PostingList],
    scorers: &[Arc<dyn Scorer>],
    k: usize,
) -> Vec<DocId> {
    let mut all: BTreeSet<DocId> = BTreeSet::new();
    for pl in posting_lists {
        for e in pl {
            all.insert(e.doc_id);
        }
    }
    let mut scored: Vec<(DocId, f64)> = all
        .into_iter()
        .map(|doc_id| {
            let mut term_scores = Vec::new();
            for (pl, scorer) in posting_lists.iter().zip(scorers.iter()) {
                if let Some(e) = pl.get_entry(doc_id) {
                    let tf = if e.payload.positions.is_empty() {
                        1
                    } else {
                        e.payload.positions.len() as u64
                    };
                    term_scores.push(scorer.term_score(tf, tf, pl.len() as u64));
                }
            }
            (doc_id, scorers[0].finalize_score(&term_scores))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    scored.truncate(k);
    scored.into_iter().map(|(d, _)| d).collect()
}

fn bm25_scorer(stats: &Arc<IndexStats>) -> Arc<dyn Scorer> {
    Arc::new(BM25Scorer::new(BM25Params::default(), stats.clone()))
}

/// Generate a random corpus of `n_terms` posting lists, each of length
/// `1..=corpus_size`, with random tf in `1..=8`. Adjacent doc ids may
/// overlap, which matches the realistic case.
fn corpus_strategy() -> impl Strategy<Value = (Vec<TermPostings>, usize)> {
    (2usize..=4, 5usize..=20).prop_flat_map(|(n_terms, corpus_size)| {
        let term = proptest::collection::btree_set(1u64..=corpus_size as u64, 1..=corpus_size)
            .prop_flat_map(|ids| {
                let len = ids.len();
                proptest::collection::vec(1u64..=8, len..=len)
                    .prop_map(move |tfs| ids.iter().copied().zip(tfs).collect::<TermPostings>())
            });
        proptest::collection::vec(term, n_terms..=n_terms).prop_map(move |t| (t, corpus_size))
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        ..ProptestConfig::default()
    })]

    /// WAND top-k matches exhaustive scoring on every randomly-drawn
    /// (corpus, query) pair. We compare doc-id sets at each rank
    /// position so a swap caused by score ties is OK as long as the
    /// resulting set is identical.
    #[test]
    fn wand_top_k_equals_exhaustive((terms_tfs, corpus_size) in corpus_strategy()) {
        let stats = {
            let mut s = IndexStats::default();
            s.total_docs = corpus_size as u64;
            s.avg_doc_length = 4.0;
            Arc::new(s)
        };
        let posting_lists: Vec<PostingList> = terms_tfs.iter().map(pl_from_tfs).collect();
        let scorers: Vec<Arc<dyn Scorer>> =
            (0..terms_tfs.len()).map(|_| bm25_scorer(&stats)).collect();

        let k = (corpus_size / 2).clamp(1, 10);
        let q = WANDQuery::new(
            posting_lists.clone(),
            scorers.clone(),
            (0..terms_tfs.len()).map(|_| FIELD.into()).collect(),
            (0..terms_tfs.len()).map(|i| format!("term_{i}")).collect(),
            k,
        );
        let wand_result = WANDScorer::new(&q, None).score_top_k();
        let wand_ids: Vec<DocId> = wand_result.top_k.iter().map(|e| e.doc_id).collect();
        let exhaustive = exhaustive_top_k(&posting_lists, &scorers, k);

        let wand_set: BTreeSet<_> = wand_ids.iter().copied().collect();
        let ex_set: BTreeSet<_> = exhaustive.iter().copied().collect();
        prop_assert_eq!(
            wand_set,
            ex_set,
            "WAND top-k diverged from exhaustive: wand={:?}, ex={:?}",
            wand_ids,
            exhaustive,
        );
    }

    /// Block-Max WAND top-k matches exhaustive scoring on every
    /// randomly-drawn (corpus, query) pair.
    #[test]
    fn bmw_top_k_equals_exhaustive((terms_tfs, corpus_size) in corpus_strategy()) {
        let stats = {
            let mut s = IndexStats::default();
            s.total_docs = corpus_size as u64;
            s.avg_doc_length = 4.0;
            Arc::new(s)
        };
        let posting_lists: Vec<PostingList> = terms_tfs.iter().map(pl_from_tfs).collect();
        let scorers: Vec<Arc<dyn Scorer>> =
            (0..terms_tfs.len()).map(|_| bm25_scorer(&stats)).collect();
        let k = (corpus_size / 2).clamp(1, 10);

        let mut block_max = BlockMaxIndex::new(8);
        for (i, pl) in posting_lists.iter().enumerate() {
            let scorer = BM25Scorer::new(BM25Params::default(), stats.clone());
            block_max.build(pl, &scorer, FIELD, &format!("term_{i}"), TABLE);
        }

        let q = WANDQuery::new(
            posting_lists.clone(),
            scorers.clone(),
            (0..terms_tfs.len()).map(|_| FIELD.into()).collect(),
            (0..terms_tfs.len()).map(|i| format!("term_{i}")).collect(),
            k,
        );
        let bmw = BlockMaxWANDScorer::new(&q, None, &block_max, TABLE);
        let bmw_result = bmw.score_top_k();
        let bmw_ids: Vec<DocId> = bmw_result.top_k.iter().map(|e| e.doc_id).collect();
        let exhaustive = exhaustive_top_k(&posting_lists, &scorers, k);

        let bmw_set: BTreeSet<_> = bmw_ids.iter().copied().collect();
        let ex_set: BTreeSet<_> = exhaustive.iter().copied().collect();
        prop_assert_eq!(
            bmw_set,
            ex_set,
            "BMW top-k diverged from exhaustive: bmw={:?}, ex={:?}",
            bmw_ids,
            exhaustive,
        );
    }
}
