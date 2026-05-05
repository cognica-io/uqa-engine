//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-paradigm operator tests.

use std::collections::BTreeMap;

use uqa_core::{Edge, Value, Vertex, VertexId};
use uqa_graph::{
    Document, EdgePattern, GraphPattern, GraphStore, MemoryGraphStore, SemanticGraphSearch,
    TextToGraph, ToGraph, VectorEnhancedMatch, VertexEmbedding, VertexPattern, VertexPredicate,
};

fn vec_value(values: &[f64]) -> Value {
    Value::List(values.iter().map(|v| Value::Float(*v)).collect())
}

#[test]
fn to_graph_builds_vertices_and_edges() {
    let docs = vec![
        Document {
            doc_id: 1,
            fields: BTreeMap::from([
                ("title".into(), Value::Str("alpha".into())),
                (
                    "links".into(),
                    Value::List(vec![Value::Int(2), Value::Int(3)]),
                ),
            ]),
        },
        Document {
            doc_id: 2,
            fields: BTreeMap::from([("title".into(), Value::Str("beta".into()))]),
        },
        Document {
            doc_id: 3,
            fields: BTreeMap::from([("title".into(), Value::Str("gamma".into()))]),
        },
    ];
    let g = ToGraph::new(docs).execute();
    let vids: Vec<VertexId> = g.vertex_ids_in_graph("default").into_iter().collect();
    assert_eq!(vids, vec![1, 2, 3]);
    let v1 = g.get_vertex(1).unwrap();
    assert_eq!(
        v1.properties.get("title"),
        Some(&Value::Str("alpha".into()))
    );
    // Vertex 1 should have 2 outgoing edges and no in-edges; vertex 2 and 3 should have one in-edge each.
    assert_eq!(g.out_edge_ids(1, "default").len(), 2);
    assert_eq!(g.in_edge_ids(2, "default").len(), 1);
}

#[test]
fn text_to_graph_creates_token_cooccurrence() {
    let docs = vec![
        Document {
            doc_id: 1,
            fields: BTreeMap::from([("text".into(), Value::Str("alpha beta gamma".into()))]),
        },
        Document {
            doc_id: 2,
            fields: BTreeMap::from([("text".into(), Value::Str("alpha gamma".into()))]),
        },
    ];
    let g = TextToGraph::new(docs).execute();
    // 3 unique tokens.
    let vids: Vec<VertexId> = g.vertex_ids_in_graph("default").into_iter().collect();
    assert_eq!(vids.len(), 3);
    // alpha-gamma should appear in both docs (window=0 -> all-pairs).
    let edges = g.edges_in_graph("default");
    let weights: Vec<i64> = edges
        .iter()
        .filter_map(|e| match e.properties.get("weight") {
            Some(Value::Int(w)) => Some(*w),
            _ => None,
        })
        .collect();
    assert!(weights.iter().any(|w| *w >= 2), "no edge with weight>=2");
}

#[test]
fn vertex_embedding_scores_by_cosine() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    let mut a = Vertex::new(1, "Doc");
    a.properties
        .insert("embedding".into(), vec_value(&[1.0, 0.0]));
    let mut b = Vertex::new(2, "Doc");
    b.properties
        .insert("embedding".into(), vec_value(&[0.0, 1.0]));
    let mut c = Vertex::new(3, "Doc");
    c.properties
        .insert("embedding".into(), vec_value(&[0.7, 0.7]));
    g.add_vertex(a, "g");
    g.add_vertex(b, "g");
    g.add_vertex(c, "g");

    let query = vec![1.0, 0.0];
    let result = VertexEmbedding::new("g", query).threshold(0.5).execute(&g);
    let entries: BTreeMap<VertexId, f64> = result
        .entries()
        .iter()
        .map(|e| (e.doc_id, e.payload.score))
        .collect();
    assert!(entries.contains_key(&1));
    assert!(entries.contains_key(&3));
    assert!(!entries.contains_key(&2));
    assert!((entries[&1] - 1.0).abs() < 1e-6);
}

#[test]
fn semantic_graph_search_filters_neighbors_by_similarity() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    let mut a = Vertex::new(1, "Doc");
    a.properties
        .insert("embedding".into(), vec_value(&[1.0, 0.0]));
    let mut b = Vertex::new(2, "Doc");
    b.properties
        .insert("embedding".into(), vec_value(&[0.9, 0.1]));
    let mut c = Vertex::new(3, "Doc");
    c.properties
        .insert("embedding".into(), vec_value(&[0.0, 1.0]));
    g.add_vertex(a, "g");
    g.add_vertex(b, "g");
    g.add_vertex(c, "g");
    g.add_edge(Edge::new(10, 1, 2, "link"), "g");
    g.add_edge(Edge::new(11, 1, 3, "link"), "g");

    let query = vec![1.0, 0.0];
    let result = SemanticGraphSearch::new("g", 1, query)
        .label("link")
        .max_hops(1)
        .threshold(0.5)
        .execute(&g);
    let ids: Vec<VertexId> = result.inner().doc_ids().collect();
    // Vertex 1 (start, sim=1.0), vertex 2 (sim≈0.9) included; vertex 3 (sim=0) excluded.
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    assert!(!ids.contains(&3));
}

#[test]
fn vector_enhanced_match_filters_pattern_results() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    let mut alice = Vertex::new(1, "Person");
    alice
        .properties
        .insert("embedding".into(), vec_value(&[1.0, 0.0]));
    let mut bob = Vertex::new(2, "Person");
    bob.properties
        .insert("embedding".into(), vec_value(&[0.0, 1.0]));
    let mut carol = Vertex::new(3, "Person");
    carol
        .properties
        .insert("embedding".into(), vec_value(&[0.95, 0.05]));
    g.add_vertex(alice, "g");
    g.add_vertex(bob, "g");
    g.add_vertex(carol, "g");
    g.add_edge(Edge::new(10, 1, 2, "knows"), "g");
    g.add_edge(Edge::new(11, 1, 3, "knows"), "g");

    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a").with(VertexPredicate::LabelEq("Person".into())))
        .add_vertex(VertexPattern::new("b").with(VertexPredicate::LabelEq("Person".into())))
        .add_edge(EdgePattern::new("a", "b").with_label("knows"));
    let query = vec![1.0, 0.0];
    let result = VectorEnhancedMatch::new("g", pattern, query, "b")
        .threshold(0.5)
        .execute(&g);
    // Only the (1, 3) match remains: b=carol with similarity ~0.998.
    assert_eq!(result.inner().len(), 1);
    let entry = &result.inner().entries()[0];
    assert!(entry.payload.score > 0.95);
}
