//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tests for `GraphEmbedding`, `PathIndex` / `LabelIndex`, and
//! `IncrementalPatternMatcher`.

use uqa_core::{Edge, Value, Vertex, VertexId};
use uqa_graph::{
    EdgePattern, GraphDelta, GraphEmbedding, GraphPattern, GraphStore, IncrementalPatternMatcher,
    LabelIndex, MemoryGraphStore, PathIndex, VertexPattern, VertexPredicate,
};

fn corpus() -> MemoryGraphStore {
    // 1 -knows-> 2 -knows-> 3
    //  \-likes-> 4
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    for v in 1..=4 {
        g.add_vertex(Vertex::new(v, "Person"), "g").unwrap();
    }
    g.add_edge(Edge::new(10, 1, 2, "knows"), "g").unwrap();
    g.add_edge(Edge::new(11, 2, 3, "knows"), "g").unwrap();
    g.add_edge(Edge::new(12, 1, 4, "likes"), "g").unwrap();
    g
}

#[test]
fn graph_embedding_writes_l2_normalized_vector() {
    let g = corpus();
    let result = GraphEmbedding::new("g")
        .dimensions(8)
        .k_layers(2)
        .execute(&g)
        .unwrap();
    assert_eq!(result.inner().len(), 4);
    for entry in result.inner().entries() {
        let v = entry
            .payload
            .fields
            .get("_embedding")
            .expect("missing _embedding");
        let Value::List(items) = v else {
            panic!("embedding not list");
        };
        assert_eq!(items.len(), 8);
        let norm: f64 = items
            .iter()
            .map(|x| match x {
                Value::Float(f) => f * f,
                _ => 0.0,
            })
            .sum::<f64>()
            .sqrt();
        // Either the embedding is all zero (degenerate vertex), or
        // normalized to unit length within tolerance.
        assert!(norm < 1e-6 || (norm - 1.0).abs() < 1e-6);
    }
}

#[test]
fn graph_embedding_rejects_unbounded_dimensions_and_layers() {
    let g = corpus();
    assert!(GraphEmbedding::new("g")
        .dimensions(usize::MAX)
        .execute(&g)
        .is_err());
    assert!(GraphEmbedding::new("g").dimensions(0).execute(&g).is_err());
    assert!(GraphEmbedding::new("g")
        .k_layers(u32::MAX)
        .execute(&g)
        .is_err());
}

#[test]
fn label_index_counts_and_groups_edges() {
    let g = corpus();
    let idx = LabelIndex::build(&g, "g").unwrap();
    let mut labels = idx.labels();
    labels.sort();
    assert_eq!(labels, vec!["knows".to_string(), "likes".to_string()]);
    assert_eq!(idx.label_count("knows"), 2);
    assert_eq!(idx.label_count("likes"), 1);
    let knows_vertices = idx.vertices_by_label("knows").unwrap();
    assert!(knows_vertices.contains(&1));
    assert!(knows_vertices.contains(&2));
    assert!(knows_vertices.contains(&3));
    assert!(!knows_vertices.contains(&4));
}

#[test]
fn path_index_reaches_pairs_per_label_sequence() {
    let g = corpus();
    let idx = PathIndex::build(
        &g,
        "g",
        &[vec!["knows".into()], vec!["knows".into(), "knows".into()]],
    )
    .unwrap();
    assert!(idx.has_path(&["knows".into()]));
    let one_hop = idx.lookup(&["knows".into()]).unwrap();
    assert!(one_hop.contains(&(1, 2)));
    assert!(one_hop.contains(&(2, 3)));
    let two_hop = idx.lookup(&["knows".into(), "knows".into()]).unwrap();
    assert!(two_hop.contains(&(1, 3)));
}

#[test]
fn incremental_matcher_drops_invalidated_and_picks_up_new() {
    let mut g = corpus();
    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a").with(VertexPredicate::LabelEq("Person".into())))
        .add_vertex(VertexPattern::new("b").with(VertexPredicate::LabelEq("Person".into())))
        .add_edge(EdgePattern::new("a", "b").with_label("knows"));

    let mut matcher = IncrementalPatternMatcher::new(pattern, "g");
    matcher.seed(&g).unwrap();
    let initial = matcher.matches().clone();
    // Two `knows` pairs: {1,2} and {2,3}.
    assert_eq!(initial.len(), 2);

    // Add a new vertex 5 and edge 4 -> 5 (knows). Only the new pair
    // should be added; existing matches survive.
    g.add_vertex(Vertex::new(5, "Person"), "g").unwrap();
    g.add_edge(Edge::new(20, 4, 5, "knows"), "g").unwrap();
    let mut delta = GraphDelta::new();
    delta.add_vertex(Vertex::new(5, "Person"));
    delta.add_edge(Edge::new(20, 4, 5, "knows"));
    let updated = matcher.update(&g, &delta).unwrap().clone();
    assert_eq!(updated.len(), 3);
    let pair_45 = updated.iter().find(|m| m.contains(&4) && m.contains(&5));
    assert!(pair_45.is_some(), "{updated:?} missing 4-5 match");

    // The original pairs survive (since vertices 1,2,3 weren't touched).
    let pair_12: Vec<&Vec<VertexId>> = updated
        .iter()
        .filter(|m| m.contains(&1) && m.contains(&2))
        .collect();
    assert_eq!(pair_12.len(), 1);

    // A remove-by-id delta carries no endpoint snapshot. The matcher must
    // perform an exact full refresh after the edge has been deleted instead
    // of retaining the stale 4 -> 5 match.
    g.remove_edge(20, "g").unwrap();
    let mut removal = GraphDelta::new();
    removal.remove_edge(20);
    let refreshed = matcher.update(&g, &removal).unwrap();
    assert_eq!(refreshed.len(), 2);
    assert!(!refreshed
        .iter()
        .any(|vertices| vertices.contains(&4) && vertices.contains(&5)));
}
