//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of `uqa/tests/test_functor.py`.

use std::collections::BTreeMap;

use uqa_core::{Payload, PostingEntry, PostingList};
use uqa_graph::{
    GraphPayload, GraphPostingList, GraphToRelationalFunctor, RelationalToGraphFunctor,
    TextToVectorFunctor,
};

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

fn graph_posting_list(entries: &[(u64, f64)]) -> GraphPostingList {
    let posting = posting_list(entries);
    let all_vids: Vec<u64> = entries.iter().map(|(doc_id, _)| *doc_id).collect();
    let graph_payloads = entries
        .iter()
        .map(|(doc_id, score)| {
            (
                *doc_id,
                GraphPayload {
                    subgraph_vertices: all_vids.clone(),
                    subgraph_edges: Vec::new(),
                    graph_name: String::new(),
                    score_override: Some(*score),
                },
            )
        })
        .collect();
    GraphPostingList::from_parts(posting, graph_payloads)
}

fn doc_ids(pl: &PostingList) -> Vec<u64> {
    pl.entries().iter().map(|entry| entry.doc_id).collect()
}

#[test]
fn graph_to_relational_identity() {
    let empty = GraphPostingList::new();
    let result = GraphToRelationalFunctor::map_object(empty);
    assert_eq!(result.len(), 0);
}

#[test]
fn graph_to_relational_map_object() {
    let graph = graph_posting_list(&[(1, 0.9), (3, 0.7), (5, 0.5)]);
    let result = GraphToRelationalFunctor::map_object(graph);
    assert_eq!(result.len(), 3);
    assert_eq!(doc_ids(&result), vec![1, 3, 5]);
    for entry in result.entries() {
        assert!(entry.payload.score > 0.0);
        assert!(entry.payload.fields.contains_key("_graph_vertices"));
    }
}

#[test]
fn relational_to_graph_map_object() {
    let functor = RelationalToGraphFunctor::default();
    let pl = posting_list(&[(2, 0.8), (4, 0.6), (6, 0.4)]);
    let result = functor.map_object(pl);
    assert_eq!(result.len(), 3);
    assert_eq!(doc_ids(result.inner()), vec![2, 4, 6]);
    for doc_id in [2, 4, 6] {
        let payload = result.get_graph_payload(doc_id).unwrap();
        assert_eq!(payload.subgraph_vertices, vec![2, 4, 6]);
        assert!(payload.subgraph_edges.is_empty());
    }
}

#[test]
fn relational_to_graph_identity() {
    let functor = RelationalToGraphFunctor::default();
    let result = functor.map_object(PostingList::new());
    assert_eq!(result.len(), 0);
}

#[test]
fn text_to_vector_map_object() {
    let functor = TextToVectorFunctor::new(64);
    let pl = text_posting_list(&[
        (1, vec![0, 5, 10], 1.0),
        (2, vec![3], 1.0),
        (3, vec![1, 7], 0.5),
    ]);
    let result = functor.map_object(pl.clone());
    assert_eq!(result.len(), 3);
    let scores: BTreeMap<u64, f64> = result
        .entries()
        .iter()
        .map(|entry| (entry.doc_id, entry.payload.score))
        .collect();
    assert_eq!(scores[&1], 1.0);
    assert!((scores[&2] - 1.0 / 3.0).abs() < 1e-9);
    assert!((scores[&3] - 1.0 / 3.0).abs() < 1e-9);
    for entry in result.entries() {
        let original = pl
            .entries()
            .iter()
            .find(|candidate| candidate.doc_id == entry.doc_id)
            .unwrap();
        assert_eq!(entry.payload.positions, original.payload.positions);
    }
}

#[test]
fn text_to_vector_empty() {
    let functor = TextToVectorFunctor::default();
    let result = functor.map_object(PostingList::new());
    assert_eq!(result.len(), 0);
}

#[test]
fn identity_law_graph_relational() {
    let graph = graph_posting_list(&[(10, 0.5), (20, 0.3)]);
    let lhs = GraphToRelationalFunctor::map_object(graph.clone());
    let rhs = GraphToRelationalFunctor::map_object(graph);
    assert_eq!(doc_ids(&lhs), doc_ids(&rhs));
    for (left, right) in lhs.entries().iter().zip(rhs.entries()) {
        assert_eq!(left.doc_id, right.doc_id);
        assert!((left.payload.score - right.payload.score).abs() < 1e-9);
    }
}

#[test]
fn composition_law_graph_relational() {
    let a = graph_posting_list(&[(1, 0.9), (2, 0.8), (3, 0.7)]);
    let b = graph_posting_list(&[(2, 0.6), (3, 0.5), (4, 0.4)]);
    let c = graph_posting_list(&[(3, 0.3), (4, 0.2), (5, 0.1)]);

    let composed_src = a.intersect(&b).union(&c);
    let lhs = GraphToRelationalFunctor::map_object(composed_src);

    let fa = GraphToRelationalFunctor::map_object(a);
    let fb = GraphToRelationalFunctor::map_object(b);
    let fc = GraphToRelationalFunctor::map_object(c);
    let rhs = fa.intersect(&fb).union(&fc);

    assert_eq!(doc_ids(&lhs), doc_ids(&rhs));
}

#[test]
fn roundtrip_relational_graph() {
    let original = posting_list(&[(1, 0.9), (3, 0.7), (5, 0.5), (7, 0.3)]);
    let graph = RelationalToGraphFunctor::default().map_object(original.clone());
    let roundtripped = GraphToRelationalFunctor::map_object(graph);
    assert_eq!(doc_ids(&original), doc_ids(&roundtripped));
}
