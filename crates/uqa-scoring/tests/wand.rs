//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! WAND / Block-Max-WAND exactness (Master plan Section 2.3,
//! Theorem 4 from the master plan).
//!
//! Generated posting lists exercise both physical input forms and both BM25
//! scorers against exhaustive scoring, including exact ties and score bits.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use proptest::prelude::*;
use uqa_core::{DocId, IndexStats, Payload, PostingEntry, PostingList};
use uqa_scoring::{
    rank_top_k, BM25Params, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer, BlockMaxWANDScorer,
    CursorBlockMaxWANDScorer, CursorWANDQuery, CursorWANDScorer, Scorer, WANDQuery, WANDScorer,
};
use uqa_storage::{BlockMaxIndex, MaterializedPostingCursor, PostingCursor, PostingScore};

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
) -> Vec<(DocId, u64)> {
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
    scored
        .into_iter()
        .map(|(doc, score)| (doc, score.to_bits()))
        .collect()
}

/// Generate a random corpus of `n_terms` posting lists, each of length
/// `1..=corpus_size`, with random tf in `1..=8`. Adjacent doc ids may
/// overlap, which matches the realistic case.
fn corpus_strategy() -> impl Strategy<Value = (Vec<TermPostings>, usize)> {
    (1usize..=4, 5usize..=20).prop_flat_map(|(n_terms, corpus_size)| {
        let term = proptest::collection::btree_set(1u64..=corpus_size as u64, 1..=corpus_size)
            .prop_flat_map(|ids| {
                let len = ids.len();
                proptest::collection::vec(1u64..=8, len..=len)
                    .prop_map(move |tfs| ids.iter().copied().zip(tfs).collect::<TermPostings>())
            });
        proptest::collection::vec(term, n_terms..=n_terms).prop_map(move |t| (t, corpus_size))
    })
}

fn check_ranked_results(
    terms: &[TermPostings],
    corpus_size: usize,
    block_max: bool,
) -> Result<(), TestCaseError> {
    let mut stats = IndexStats::default();
    stats.total_docs = corpus_size as u64;
    stats.avg_doc_length = 4.0;
    let stats = Arc::new(stats);
    let lists: Vec<_> = terms.iter().map(pl_from_tfs).collect();
    let keys: Vec<_> = (0..terms.len()).map(|i| format!("term_{i}")).collect();
    let fields = vec![FIELD.to_owned(); terms.len()];
    let raw = Arc::new(BM25Scorer::new(BM25Params::default(), stats.clone()));
    let bayesian = Arc::new(
        BayesianBM25Scorer::new(
            BayesianBM25Params {
                alpha: 1.4,
                beta: 0.7,
                base_rate: 0.1,
                ..Default::default()
            },
            stats,
        )
        .unwrap(),
    );
    let mut bounds = BlockMaxIndex::new(8).unwrap();
    if block_max {
        for (list, key) in lists.iter().zip(&keys) {
            bounds.build(list, raw.as_ref(), FIELD, key, TABLE).unwrap();
        }
    }
    let k = (corpus_size / 2).clamp(1, 10);
    for model in [raw as Arc<dyn Scorer>, bayesian] {
        let scorers = vec![model; terms.len()];
        let expected = exhaustive_top_k(&lists, &scorers, k);
        let query = WANDQuery::new(
            lists.clone(),
            scorers.clone(),
            fields.clone(),
            keys.clone(),
            k,
        )
        .unwrap();
        let cursors = terms
            .iter()
            .map(|postings| {
                let entries = postings
                    .iter()
                    .map(|&(doc_id, term_freq)| PostingScore {
                        doc_id,
                        term_freq,
                        doc_length: term_freq,
                    })
                    .collect();
                Box::new(MaterializedPostingCursor::new(entries).unwrap()) as Box<dyn PostingCursor>
            })
            .collect();
        let cursor_query =
            CursorWANDQuery::new(cursors, scorers, fields.clone(), keys.clone(), k).unwrap();
        let results = if block_max {
            [
                BlockMaxWANDScorer::new(&query, None, &bounds, TABLE)
                    .score_top_k()
                    .unwrap(),
                CursorBlockMaxWANDScorer::new(&cursor_query, &bounds, TABLE)
                    .score_top_k()
                    .unwrap(),
            ]
        } else {
            [
                WANDScorer::new(&query, None).score_top_k().unwrap(),
                CursorWANDScorer::new(&cursor_query).score_top_k().unwrap(),
            ]
        };
        for result in results {
            let actual: Vec<_> = rank_top_k(&result.top_k, k)
                .into_iter()
                .map(|entry| (entry.doc_id, entry.score.to_bits()))
                .collect();
            prop_assert_eq!(&actual, &expected);
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn wand_top_k_equals_exhaustive((terms, size) in corpus_strategy()) {
        check_ranked_results(&terms,size,false)?;
    }

    #[test]
    fn bmw_top_k_equals_exhaustive((terms, size) in corpus_strategy()) {
        check_ranked_results(&terms,size,true)?;
    }
}
