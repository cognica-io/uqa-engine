//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cmp::Ordering;
use std::sync::Arc;

use crate::Scorer;

use super::*;
use uqa_core::{DocId, IndexStats, Payload, PostingEntry, PostingList};
use uqa_storage::{BlockMaxIndex, MaterializedPostingCursor, PostingCursor, PostingScore};

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

fn score_cursor(tfs: &[(DocId, u64)]) -> Box<dyn PostingCursor> {
    Box::new(
        MaterializedPostingCursor::new(
            tfs.iter()
                .map(|&(doc_id, term_freq)| PostingScore {
                    doc_id,
                    term_freq,
                    doc_length: term_freq,
                })
                .collect(),
        )
        .unwrap(),
    )
}

fn assert_same_scores(left: &PostingList, right: &PostingList) {
    assert_eq!(left.len(), right.len());
    for (left, right) in left.iter().zip(right) {
        assert_eq!(left.doc_id, right.doc_id);
        assert!((left.payload.score - right.payload.score).abs() < 1e-12);
    }
}

struct InvalidScorer;

impl Scorer for InvalidScorer {
    fn idf(&self, _doc_freq: u64) -> f64 {
        f64::NAN
    }

    fn term_score(&self, _term_freq: u64, _doc_length: u64, _doc_freq: u64) -> f64 {
        f64::NAN
    }

    fn term_score_with_idf(&self, _term_freq: u64, _doc_length: u64, _idf_value: f64) -> f64 {
        f64::NAN
    }

    fn finalize_score(&self, _term_scores: &[f64]) -> f64 {
        f64::NAN
    }

    fn term_upper_bound(&self, _doc_freq: u64) -> f64 {
        f64::NAN
    }
}

#[test]
fn wand_rejects_mismatched_shapes_and_non_finite_bounds() {
    let posting_list = pl_from_tfs(&[(1, 1)]);
    assert!(WANDQuery::new(
        vec![posting_list.clone()],
        Vec::new(),
        vec!["body".into()],
        vec!["term".into()],
        1,
    )
    .is_err());

    let query = WANDQuery::new(
        vec![posting_list],
        vec![Arc::new(InvalidScorer)],
        vec!["body".into()],
        vec!["term".into()],
        1,
    )
    .unwrap();
    assert!(WANDScorer::new(&query, None).score_top_k().is_err());
}

#[test]
fn zero_k_returns_no_results() {
    let mut stats = IndexStats::default();
    stats.total_docs = 10;
    stats.avg_doc_length = 5.0;
    let query = WANDQuery::new(
        vec![pl_from_tfs(&[(1, 1)])],
        vec![bm25(Arc::new(stats))],
        vec!["body".into()],
        vec!["term".into()],
        0,
    )
    .unwrap();
    assert!(WANDScorer::new(&query, None)
        .score_top_k()
        .unwrap()
        .top_k
        .is_empty());
}

#[test]
fn candidate_union_merges_sorted_postings_without_materializing_ids() {
    let postings = vec![
        pl_from_tfs(&[(1, 1), (4, 1), (9, 1)]),
        pl_from_tfs(&[(2, 1), (4, 1), (7, 1)]),
        pl_from_tfs(&[(1, 1), (8, 1), (9, 1)]),
    ];
    assert_eq!(candidate_union(&postings).unwrap(), 6);
    assert_eq!(candidate_union(&[]).unwrap(), 0);
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
    )
    .unwrap();
    let wand = WANDScorer::new(&q, None);
    let result = wand.score_top_k().unwrap();

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
fn score_cursor_wand_and_bmw_match_materialized_wand() {
    let mut stats = IndexStats::default();
    stats.total_docs = 12;
    stats.avg_doc_length = 3.0;
    let stats = Arc::new(stats);
    let rust = [(1, 3), (2, 1), (4, 2), (5, 5), (8, 1)];
    let lang = [(1, 1), (3, 4), (4, 1), (6, 2), (8, 3)];
    let posting_lists = vec![pl_from_tfs(&rust), pl_from_tfs(&lang)];
    let scorers = vec![bm25(stats.clone()), bm25(stats)];
    let materialized = WANDQuery::new(
        posting_lists.clone(),
        scorers.clone(),
        vec!["title".into(), "title".into()],
        vec!["rust".into(), "lang".into()],
        3,
    )
    .unwrap();
    let expected = WANDScorer::new(&materialized, None).score_top_k().unwrap();
    let cursors = CursorWANDQuery::new(
        vec![score_cursor(&rust), score_cursor(&lang)],
        scorers.clone(),
        vec!["title".into(), "title".into()],
        vec!["rust".into(), "lang".into()],
        3,
    )
    .unwrap();
    let cursor_wand = CursorWANDScorer::new(&cursors).score_top_k().unwrap();
    assert_same_scores(&cursor_wand.top_k, &expected.top_k);

    let mut block_max = BlockMaxIndex::new(2).unwrap();
    for ((term, posting), scorer) in ["rust", "lang"]
        .into_iter()
        .zip(&posting_lists)
        .zip(&scorers)
    {
        let doc_freq = posting.len() as u64;
        let block_upper_bounds = posting
            .entries()
            .chunks(2)
            .map(|block| {
                block
                    .iter()
                    .map(|entry| {
                        let term_freq = entry.payload.positions.len() as u64;
                        scorer.term_score(term_freq, term_freq, doc_freq)
                    })
                    .fold(0.0_f64, f64::max)
            })
            .collect();
        block_max
            .set_block_maxes("articles", "title", term, block_upper_bounds)
            .unwrap();
    }
    let cursor_bmw = CursorBlockMaxWANDScorer::new(&cursors, &block_max, "articles")
        .score_top_k()
        .unwrap();
    assert_same_scores(&cursor_bmw.top_k, &expected.top_k);
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
        .map(|_| {
            Arc::new(BayesianBM25Scorer::new(params, stats.clone()).unwrap()) as Arc<dyn Scorer>
        })
        .collect();
    let query = WANDQuery::new(
        posting_lists.clone(),
        scorers.clone(),
        vec!["title".into(), "title".into()],
        vec!["rust".into(), "language".into()],
        3,
    )
    .unwrap();
    let result = WANDScorer::new(&query, None).score_top_k().unwrap();

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
    for ((actual_doc, actual_score), (expected_doc, expected_score)) in actual.iter().zip(&expected)
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
    a.record(1.0, 0.8).unwrap();
    a.record(2.0, 1.0).unwrap();
    // ratios: 0.8, 0.5; mean 0.65
    assert!((a.tightness_ratio() - 0.65).abs() < 1e-9);
}
