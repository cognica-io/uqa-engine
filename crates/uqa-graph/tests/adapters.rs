//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the representation adapters. No categorical functor laws are
//! claimed because these types do not map operators/morphisms.

use std::collections::BTreeMap;

use uqa_core::{Payload, PostingEntry, PostingList};
use uqa_graph::{GraphPostingCodec, PostingToGraphAdapter, TextTfScoreNormalizer};

fn posting_list(entries: &[(u64, f64)]) -> PostingList {
    PostingList::from_sorted_unchecked(
        entries
            .iter()
            .map(|(doc_id, score)| PostingEntry::new(*doc_id, Payload::with_score(*score)))
            .collect(),
    )
}

fn text_posting_list(entries: &[(u64, Vec<u32>, f64)]) -> PostingList {
    PostingList::from_sorted_unchecked(
        entries
            .iter()
            .map(|(doc_id, positions, score)| {
                PostingEntry::new(
                    *doc_id,
                    Payload {
                        positions: positions.clone(),
                        score: *score,
                        fields: BTreeMap::new(),
                    },
                )
            })
            .collect(),
    )
}

fn doc_ids(posting: &PostingList) -> Vec<u64> {
    posting.iter().map(|entry| entry.doc_id).collect()
}

#[test]
fn posting_to_graph_attaches_only_declared_vertex_context() {
    let posting = posting_list(&[(2, 0.8), (4, 0.6), (6, 0.4)]);
    let graph = PostingToGraphAdapter
        .attach_shared_vertex_context(posting)
        .unwrap();
    assert_eq!(doc_ids(graph.inner()), vec![2, 4, 6]);
    for doc_id in [2, 4, 6] {
        let payload = graph.get_graph_payload(doc_id).unwrap();
        assert_eq!(payload.subgraph_vertices, vec![2, 4, 6]);
        assert!(payload.subgraph_edges.is_empty());
    }
}

#[test]
fn graph_codec_preserves_payload_instead_of_claiming_to_strip_it() {
    let graph = PostingToGraphAdapter
        .attach_shared_vertex_context(posting_list(&[(1, 0.9), (3, 0.7), (5, 0.5)]))
        .unwrap();
    let encoded = GraphPostingCodec::encode(graph.clone());
    assert_eq!(doc_ids(&encoded), vec![1, 3, 5]);
    assert!(encoded
        .iter()
        .all(|entry| entry.payload.fields.contains_key("_graph_vertices")));
    assert_eq!(GraphPostingCodec::decode(&encoded), graph);
}

#[test]
fn text_tf_score_normalizer_has_no_unused_vector_dimension_contract() {
    let posting = text_posting_list(&[
        (1, vec![0, 5, 10], 1.0),
        (2, vec![3], 1.0),
        (3, vec![1, 7], 0.5),
    ]);
    let result = TextTfScoreNormalizer.normalize(posting.clone());
    let scores: BTreeMap<u64, f64> = result
        .iter()
        .map(|entry| (entry.doc_id, entry.payload.score))
        .collect();
    assert_eq!(scores[&1], 1.0);
    assert!((scores[&2] - 1.0 / 3.0).abs() < 1e-9);
    assert!((scores[&3] - 1.0 / 3.0).abs() < 1e-9);
    for entry in &result {
        let original = posting.get_entry(entry.doc_id).unwrap();
        assert_eq!(entry.payload.positions, original.payload.positions);
    }
}

#[test]
fn adapters_preserve_empty_inputs() {
    let graph = PostingToGraphAdapter
        .attach_shared_vertex_context(PostingList::new())
        .unwrap();
    assert!(graph.is_empty());
    assert!(GraphPostingCodec::encode(graph).is_empty());
    assert!(TextTfScoreNormalizer
        .normalize(PostingList::new())
        .is_empty());
}
