//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Phrase adjacency, graph alternatives, cursor support, and execution limits.

use super::*;
use std::collections::BTreeMap;
use uqa_analysis::{whitespace_analyzer, Analyzer};
use uqa_core::CancellationToken;
use uqa_scoring::{score_text_terms, BM25Params, TextSearchAlgorithm};
use uqa_storage::{inverted_index::analyze_query_graph, MemoryInvertedIndex};

fn edge(term: &str, position: u32, length: u32) -> (TokenTermKey, TokenOccurrence) {
    (
        TokenTermKey::from_text(term),
        TokenOccurrence {
            position,
            position_length: length,
            offsets: None,
        },
    )
}

fn graph_match(
    query: &[(TokenTermKey, TokenOccurrence)],
    document: &[(TokenTermKey, TokenOccurrence)],
) -> bool {
    let cancellation = CancellationToken::new();
    let mut budget = PhraseBudget::new(1024 * 1024, &cancellation);
    let graph = QueryGraph::new(query, &mut budget).unwrap();
    let postings = graph
        .terms
        .iter()
        .map(|key| {
            let mut values = document
                .iter()
                .filter(|(term, _)| term == *key)
                .map(|(_, occurrence)| *occurrence)
                .collect::<Vec<_>>();
            values.sort_by_key(|occurrence| occurrence.position);
            values
        })
        .collect::<Vec<_>>();
    graph
        .matches(&postings, &mut Vec::new(), &mut budget)
        .unwrap()
}

#[test]
fn connected_paths_preserve_order_and_internal_holes() {
    let phrase = [edge("a", 0, 1), edge("b", 1, 1)];
    assert!(graph_match(&phrase, &[edge("a", 4, 1), edge("b", 5, 1)]));
    assert!(!graph_match(&phrase, &[edge("b", 0, 1), edge("a", 1, 1)]));
    assert!(!graph_match(&phrase, &[edge("a", 0, 1), edge("b", 2, 1)]));
    let gap = [edge("a", 2, 1), edge("b", 4, 1)];
    assert!(graph_match(&gap, &[edge("a", 0, 1), edge("b", 2, 1)]));
    assert!(!graph_match(&gap, &phrase));
}

#[test]
fn alternatives_advance_each_graph_by_its_own_edge_length() {
    let mixed = [
        edge("ab", 0, 2),
        edge("a", 0, 1),
        edge("b", 1, 1),
        edge("c", 2, 1),
    ];
    assert!(graph_match(&mixed, &[edge("ab", 4, 1), edge("c", 5, 1)]));
    assert!(graph_match(
        &mixed,
        &[edge("a", 4, 1), edge("b", 5, 1), edge("c", 6, 1)]
    ));
    assert!(graph_match(&[edge("ab", 0, 1), edge("c", 1, 1)], &mixed));
    assert!(!graph_match(&[edge("ab", 0, 1), edge("b", 1, 1)], &mixed));
    assert!(!graph_match(&mixed, &[edge("a", 0, 1), edge("c", 1, 1)]));
}

#[test]
fn disconnected_terminal_alternatives_do_not_accept_partial_paths() {
    let query = [edge("ab", 0, 2), edge("a", 0, 1)];
    assert!(!graph_match(&query, &[edge("a", 0, 1)]));
    assert!(graph_match(&query, &[edge("ab", 0, 1)]));
}

#[test]
fn repeated_terms_need_distinct_connected_occurrences() {
    let query = [edge("a", 0, 1), edge("a", 1, 1), edge("b", 2, 1)];
    assert!(!graph_match(&query, &[edge("a", 0, 1), edge("b", 1, 1)]));
    assert!(graph_match(
        &query,
        &[
            edge("a", 0, 1),
            edge("b", 1, 1),
            edge("a", 4, 1),
            edge("a", 5, 1),
            edge("b", 6, 1)
        ]
    ));
}

#[test]
fn exponentially_many_paths_share_memoized_states() {
    let mut query = Vec::new();
    let mut document = Vec::new();
    for position in 0..160 {
        query.extend([edge("a", position, 1), edge("b", position, 1)]);
        document.extend([edge("a", position, 1), edge("b", position, 1)]);
    }
    query.push(edge("last", 160, 1));
    assert!(!graph_match(&query, &document));
    document.push(edge("last", 160, 1));
    assert!(graph_match(&query, &document));
}

#[test]
fn raw_utf16_edges_are_distinct_from_replacement_characters() {
    let raw = (
        TokenTermKey::from_bytes(vec![1, 0xd8, 0x3d]).unwrap(),
        edge("", 0, 1).1,
    );
    let query = [raw.clone(), edge("b", 1, 1)];
    assert!(graph_match(&query, &[raw, edge("b", 1, 1)]));
    assert!(!graph_match(&query, &[edge("�", 0, 1), edge("b", 1, 1)]));
}

fn fixture(analyzer: Analyzer, documents: &[(DocId, &str)]) -> MemoryInvertedIndex {
    let mut index = MemoryInvertedIndex::new(analyzer);
    for &(doc_id, text) in documents {
        index
            .add_document(doc_id, BTreeMap::from([("body".into(), text.into())]))
            .unwrap();
    }
    index
}

fn search(index: &dyn InvertedIndex, text: &str) -> Vec<ScoredEntry> {
    let query =
        analyze_query_graph(&index.search_analyzer_revision("body").unwrap(), text).unwrap();
    score_phrase(
        index,
        "body",
        &query,
        &ScoringMode::BM25(BM25Params::default()),
        &mut PhraseBudget::new(1024 * 1024, &CancellationToken::new()),
    )
    .unwrap()
}

#[test]
fn score_cursors_filter_positions_before_scoring_and_retain_maximum_doc_id() {
    let index = fixture(
        whitespace_analyzer(),
        &[
            (1, "a x a b"),
            (2, "a a b"),
            (3, "a b"),
            (DocId::MAX, "a a b"),
        ],
    );
    let phrase = search(&index, "a a b");
    assert_eq!(
        phrase.iter().map(|row| row.doc_id).collect::<Vec<_>>(),
        [2, DocId::MAX]
    );
    let terms = ["a", "a", "b"].map(TokenTermKey::from_text);
    let raw = score_text_terms(
        &index,
        "docs",
        "body",
        &terms,
        &ScoringMode::BM25(BM25Params::default()),
        usize::MAX,
        TextSearchAlgorithm::Exhaustive,
    )
    .unwrap();
    for row in phrase {
        assert_eq!(
            row.score,
            raw.entries
                .iter()
                .find(|entry| entry.doc_id == row.doc_id)
                .unwrap()
                .score
        );
    }
}

#[test]
fn whole_phrase_analysis_preserves_stopword_holes_and_empty_queries() {
    let analyzer = serde_json::from_str(r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"stop","language":"none","custom_words":["the"]}]}"#).unwrap();
    let index = fixture(analyzer, &[(1, "a the b"), (2, "a b"), (3, "a x b")]);
    assert_eq!(
        search(&index, "the a the b the")
            .iter()
            .map(|row| row.doc_id)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert!(search(&index, "the").is_empty());
    assert!(search(&index, "").is_empty());
}

#[test]
fn cancellation_and_memory_limits_fail_without_partial_output() {
    let index = fixture(whitespace_analyzer(), &[(1, "a b"), (2, "a b")]);
    let query = [edge("a", 0, 1), edge("b", 1, 1)];
    let cancellation = CancellationToken::new();
    let mode = ScoringMode::BM25(BM25Params::default());
    assert!(matches!(
        score_phrase(
            &index,
            "body",
            &query,
            &mode,
            &mut PhraseBudget::new(1, &cancellation)
        ),
        Err(PhraseError::MemoryLimit { .. })
    ));
    cancellation.cancel();
    assert!(matches!(
        score_phrase(
            &index,
            "body",
            &query,
            &mode,
            &mut PhraseBudget::new(1024 * 1024, &cancellation)
        ),
        Err(PhraseError::Cancelled(_))
    ));
}
