//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    score_text_query, visit_text_query, BM25Params, BayesianBM25Params, ScoringMode,
    TextSearchAlgorithm,
};
use uqa_storage::{InvertedIndex, MemoryInvertedIndex};

fn index() -> MemoryInvertedIndex {
    let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    for (id, text) in [
        (1, "alpha"),
        (2, "alpha alpha beta"),
        (4, "beta"),
        (9, "gamma"),
    ] {
        index
            .add_document(id, [("body".into(), text.into())].into())
            .unwrap();
    }
    index
}

#[test]
fn exhaustive_text_visitor_preserves_native_score_bits_and_duplicate_terms() {
    let index = index();
    for mode in [
        ScoringMode::BM25(BM25Params::default()),
        ScoringMode::BayesianBM25(BayesianBM25Params::default()),
    ] {
        for query in ["", "absent", "alpha", "alpha beta alpha", "beta alpha beta"] {
            let mut expected = score_text_query(
                &index,
                "docs",
                "body",
                query,
                &mode,
                usize::MAX,
                TextSearchAlgorithm::Exhaustive,
            )
            .unwrap()
            .entries;
            expected.sort_by_key(|entry| entry.doc_id);
            let mut actual = Vec::new();
            assert!(
                visit_text_query(&index, "body", query, &mode, &mut |entry| {
                    actual.push(entry);
                    true
                })
                .unwrap()
            );
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(actual.doc_id, expected.doc_id);
                assert_eq!(actual.score.to_bits(), expected.score.to_bits());
            }
        }
    }
}

#[test]
fn exhaustive_text_visitor_stops_at_the_consumer_boundary() {
    let index = index();
    for query in ["alpha", "alpha beta alpha"] {
        let mut visits = 0;
        assert!(!visit_text_query(
            &index,
            "body",
            query,
            &ScoringMode::BM25(BM25Params::default()),
            &mut |entry| {
                visits += 1;
                assert_eq!(entry.doc_id, 1);
                false
            }
        )
        .unwrap());
        assert_eq!(visits, 1);
    }
}

#[test]
fn exhaustive_text_visitor_preserves_parameter_errors_before_output() {
    let index = index();
    let mode = ScoringMode::BM25(BM25Params {
        k1: -1.0,
        ..BM25Params::default()
    });
    let mut visits = 0;
    let error = visit_text_query(&index, "body", "alpha", &mode, &mut |_| {
        visits += 1;
        true
    })
    .unwrap_err();
    assert!(matches!(error, crate::TextSearchError::Parameters(_)));
    assert_eq!(visits, 0);
}

#[test]
fn exhaustive_text_visitor_borrows_large_term_support() {
    let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    for id in 0..65_536 {
        index
            .add_document(id, [("body".into(), "alpha beta".into())].into())
            .unwrap();
    }
    for query in ["alpha", "alpha beta alpha"] {
        let mut next_id = 0;
        let allocated = allocation_counter::measure(|| {
            assert!(visit_text_query(
                &index,
                "body",
                query,
                &ScoringMode::BM25(BM25Params::default()),
                &mut |entry| {
                    assert_eq!(entry.doc_id, next_id);
                    assert!(entry.score.is_finite() && entry.score > 0.0);
                    next_id += 1;
                    true
                },
            )
            .unwrap());
        });
        assert_eq!(next_id, 65_536);
        // Even one complete score vector needs 1MiB; retained posting reads need only cursor and term state.
        assert!(allocated.bytes_max < 128 * 1024, "{allocated:?}");
    }
}
