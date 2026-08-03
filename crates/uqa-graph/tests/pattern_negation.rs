//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph-pattern negation coverage.

use uqa_core::{Edge, Value, Vertex};
use uqa_graph::{EdgePattern, GMatch, GraphPattern, GraphStore, MemoryGraphStore, VertexPattern};

fn social_graph() -> MemoryGraphStore {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    for (id, name) in [(1, "Alice"), (2, "Bob"), (3, "Carol"), (4, "Dave")] {
        let mut vertex = Vertex::new(id, "person");
        vertex
            .properties
            .insert("name".to_string(), Value::Str(name.to_string()));
        store.add_vertex(vertex, "g").unwrap();
    }
    store.add_edge(Edge::new(10, 1, 2, "knows"), "g").unwrap();
    store.add_edge(Edge::new(11, 1, 3, "knows"), "g").unwrap();
    store.add_edge(Edge::new(12, 2, 3, "knows"), "g").unwrap();
    store.add_edge(Edge::new(13, 1, 4, "blocks"), "g").unwrap();
    store
}

fn assignment_pairs(result: &uqa_graph::GraphPostingList) -> Vec<(i64, i64)> {
    result
        .inner()
        .entries()
        .iter()
        .filter_map(
            |entry| match (entry.payload.fields.get("a"), entry.payload.fields.get("b")) {
                (Some(Value::Int(a)), Some(Value::Int(b))) => Some((*a, *b)),
                _ => None,
            },
        )
        .collect()
}

#[test]
fn positive_edge_pattern() {
    let store = social_graph();
    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a"))
        .add_vertex(VertexPattern::new("b"))
        .add_edge(EdgePattern::new("a", "b").with_label("knows"));
    let result = GMatch::new(pattern, "g").execute(&store).unwrap();
    assert_eq!(result.inner().len(), 3);
}

#[test]
fn negated_edge_basic() {
    let store = social_graph();
    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a"))
        .add_vertex(VertexPattern::new("b"))
        .add_edge(EdgePattern::new("a", "b").with_label("knows"))
        .add_edge(EdgePattern::new("a", "b").with_label("blocks").negated());
    let result = GMatch::new(pattern, "g").execute(&store).unwrap();
    assert_eq!(result.inner().len(), 3);
}

#[test]
fn negated_edge_filters_match() {
    let store = social_graph();
    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a"))
        .add_vertex(VertexPattern::new("b"))
        .add_edge(EdgePattern::new("a", "b").with_label("blocks"))
        .add_edge(EdgePattern::new("a", "b").with_label("knows").negated());
    let result = GMatch::new(pattern, "g").execute(&store).unwrap();
    let pairs = assignment_pairs(&result);
    assert_eq!(pairs, vec![(1, 4)]);
}

#[test]
fn negated_edge_removes_all() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    store.add_vertex(Vertex::new(1, "a"), "g").unwrap();
    store.add_vertex(Vertex::new(2, "b"), "g").unwrap();
    store.add_edge(Edge::new(10, 1, 2, "knows"), "g").unwrap();
    store.add_edge(Edge::new(11, 1, 2, "blocks"), "g").unwrap();

    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a"))
        .add_vertex(VertexPattern::new("b"))
        .add_edge(EdgePattern::new("a", "b").with_label("knows"))
        .add_edge(EdgePattern::new("a", "b").with_label("blocks").negated());
    let result = GMatch::new(pattern, "g").execute(&store).unwrap();
    assert_eq!(result.inner().len(), 0);
}

#[test]
fn negated_only_pattern() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    store.add_vertex(Vertex::new(1, "a"), "g").unwrap();
    store.add_vertex(Vertex::new(2, "b"), "g").unwrap();
    store.add_vertex(Vertex::new(3, "c"), "g").unwrap();
    store.add_edge(Edge::new(10, 1, 2, "e"), "g").unwrap();

    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a"))
        .add_vertex(VertexPattern::new("b"))
        .add_edge(EdgePattern::new("a", "b").with_label("e").negated());
    let result = GMatch::new(pattern, "g").execute(&store).unwrap();
    assert_eq!(result.inner().len(), 5);
}

#[test]
fn negated_edge_no_label() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    store.add_vertex(Vertex::new(1, "a"), "g").unwrap();
    store.add_vertex(Vertex::new(2, "b"), "g").unwrap();
    store.add_vertex(Vertex::new(3, "c"), "g").unwrap();
    store.add_edge(Edge::new(10, 1, 2, "e1"), "g").unwrap();

    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a"))
        .add_vertex(VertexPattern::new("b"))
        .add_edge(EdgePattern::new("a", "b").negated());
    let result = GMatch::new(pattern, "g").execute(&store).unwrap();
    assert_eq!(result.inner().len(), 5);
}

#[test]
fn negated_edge_default_false() {
    let edge = EdgePattern::new("a", "b").with_label("knows");
    assert!(!edge.negated);

    let negated = EdgePattern::new("a", "b").with_label("knows").negated();
    assert!(negated.negated);
}
