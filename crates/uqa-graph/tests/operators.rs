//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for graph operators on `MemoryGraphStore`.

use uqa_core::{Edge, Value, Vertex, VertexId};
use uqa_graph::{
    AggFn, EdgePattern, GMatch, GraphPattern, GraphStore, MemoryGraphStore, Traverse,
    VertexAggregation, VertexMatch, VertexPattern, VertexPredicate,
};

fn corpus() -> MemoryGraphStore {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    let alice = {
        let mut v = Vertex::new(1, "person");
        v.properties
            .insert("salary".to_string(), Value::Int(100_000));
        v
    };
    let bob = {
        let mut v = Vertex::new(2, "person");
        v.properties
            .insert("salary".to_string(), Value::Int(80_000));
        v
    };
    let carol = {
        let mut v = Vertex::new(3, "person");
        v.properties
            .insert("salary".to_string(), Value::Int(120_000));
        v
    };
    let acme = Vertex::new(4, "company");
    g.add_vertex(alice, "g");
    g.add_vertex(bob, "g");
    g.add_vertex(carol, "g");
    g.add_vertex(acme, "g");
    g.add_edge(Edge::new(10, 1, 2, "knows"), "g");
    g.add_edge(Edge::new(11, 2, 3, "knows"), "g");
    g.add_edge(Edge::new(12, 1, 4, "works_at"), "g");
    g.add_edge(Edge::new(13, 2, 4, "works_at"), "g");
    g
}

#[test]
fn traverse_one_hop_returns_only_direct_neighbors() {
    let g = corpus();
    let result = Traverse::new(1, "g").label("knows").max_hops(1).execute(&g);
    let ids: Vec<VertexId> = result.inner().doc_ids().collect();
    // includes start vertex 1 plus 1-hop neighbor 2.
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn traverse_two_hops_extends_frontier() {
    let g = corpus();
    let result = Traverse::new(1, "g").label("knows").max_hops(2).execute(&g);
    let mut ids: Vec<VertexId> = result.inner().doc_ids().collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn traverse_predicate_filters_neighbors() {
    let g = corpus();
    let result = Traverse::new(1, "g")
        .label("knows")
        .max_hops(2)
        .predicate(VertexPredicate::PropertyEq {
            key: "salary".into(),
            value: Value::Int(120_000),
        })
        .execute(&g);
    let mut ids: Vec<VertexId> = result.inner().doc_ids().collect();
    ids.sort_unstable();
    // Start (1) + carol (3) — bob (80k) is filtered out, so carol can't be
    // reached without bob in the frontier; predicate gates frontier extension.
    // Expected: only the start vertex remains visited.
    assert_eq!(ids, vec![1]);
}

#[test]
fn vertex_match_filters_by_label_and_property() {
    let g = corpus();
    let result = VertexMatch::new("g")
        .label("person")
        .predicate(VertexPredicate::PropertyEq {
            key: "salary".into(),
            value: Value::Int(120_000),
        })
        .execute(&g);
    let ids: Vec<VertexId> = result.inner().doc_ids().collect();
    assert_eq!(ids, vec![3]);
}

#[test]
fn gmatch_simple_two_vertex_pattern() {
    let g = corpus();
    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a").with(VertexPredicate::LabelEq("person".into())))
        .add_vertex(VertexPattern::new("b").with(VertexPredicate::LabelEq("person".into())))
        .add_edge(EdgePattern::new("a", "b").with_label("knows"));
    let result = GMatch::new(pattern, "g").execute(&g);
    // 1->2 and 2->3 each yield a single subgraph match.
    assert_eq!(result.inner().len(), 2);
    let assignments: Vec<(i64, i64)> = result
        .inner()
        .entries()
        .iter()
        .filter_map(
            |e| match (e.payload.fields.get("a"), e.payload.fields.get("b")) {
                (Some(Value::Int(a)), Some(Value::Int(b))) => Some((*a, *b)),
                _ => None,
            },
        )
        .collect();
    assert!(assignments.contains(&(1, 2)));
    assert!(assignments.contains(&(2, 3)));
}

#[test]
fn gmatch_negated_edge_excludes_existing_pairs() {
    let g = corpus();
    let pattern = GraphPattern::new()
        .add_vertex(VertexPattern::new("a").with(VertexPredicate::LabelEq("person".into())))
        .add_vertex(VertexPattern::new("b").with(VertexPredicate::LabelEq("person".into())))
        .add_edge(EdgePattern::new("a", "b").with_label("knows").negated());
    let result = GMatch::new(pattern, "g").execute(&g);
    // (1->2) and (2->3) have a knows edge, so they are excluded. All other
    // ordered pairs of distinct people remain.
    let pairs: Vec<(i64, i64)> = result
        .inner()
        .entries()
        .iter()
        .filter_map(
            |e| match (e.payload.fields.get("a"), e.payload.fields.get("b")) {
                (Some(Value::Int(a)), Some(Value::Int(b))) => Some((*a, *b)),
                _ => None,
            },
        )
        .collect();
    // 3 person vertices, ordered distinct pairs = 6, minus 2 knows edges = 4.
    assert_eq!(pairs.len(), 4);
    assert!(!pairs.contains(&(1, 2)));
    assert!(!pairs.contains(&(2, 3)));
}

#[test]
fn vertex_aggregation_sum_over_traversal() {
    let g = corpus();
    let traversed = Traverse::new(1, "g").label("knows").max_hops(2).execute(&g);
    // visited vertices: {1, 2, 3} — salaries 100k, 80k, 120k -> sum 300k.
    let agg = VertexAggregation::new(traversed, "salary", AggFn::Sum, "g").execute(&g);
    let entry = &agg.inner().entries()[0];
    let result = match entry.payload.fields.get("_vertex_agg_result") {
        Some(Value::Float(v)) => *v,
        other => panic!("expected float result, got {other:?}"),
    };
    assert!((result - 300_000.0).abs() < 1e-6);
    let count = match entry.payload.fields.get("_vertex_agg_count") {
        Some(Value::Int(n)) => *n,
        other => panic!("expected int count, got {other:?}"),
    };
    assert_eq!(count, 3);
}
