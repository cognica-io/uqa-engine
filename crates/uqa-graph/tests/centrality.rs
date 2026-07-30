//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Centrality + message-passing tests on `MemoryGraphStore`.

use uqa_core::{Edge, Value, Vertex};
use uqa_graph::{
    AggregationKind, BetweennessCentrality, GraphStore, MemoryGraphStore, MessagePassing, PageRank,
    HITS,
};

fn linear_corpus() -> MemoryGraphStore {
    // 1 -> 2 -> 3 -> 4
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    for v in 1..=4 {
        g.add_vertex(Vertex::new(v, "n"), "g").unwrap();
    }
    g.add_edge(Edge::new(10, 1, 2, "e"), "g").unwrap();
    g.add_edge(Edge::new(11, 2, 3, "e"), "g").unwrap();
    g.add_edge(Edge::new(12, 3, 4, "e"), "g").unwrap();
    g
}

fn star_corpus() -> MemoryGraphStore {
    // 1 -> 2 (hub), all others -> 2.
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    for v in 1..=4 {
        g.add_vertex(Vertex::new(v, "n"), "g").unwrap();
    }
    g.add_edge(Edge::new(10, 1, 2, "e"), "g").unwrap();
    g.add_edge(Edge::new(11, 3, 2, "e"), "g").unwrap();
    g.add_edge(Edge::new(12, 4, 2, "e"), "g").unwrap();
    g
}

#[test]
fn pagerank_assigns_higher_score_to_central_vertex_in_star() {
    let g = star_corpus();
    let result = PageRank::new("g").execute(&g).unwrap();
    let scores = score_map(&result);
    let center = scores[&2];
    for v in [1u64, 3, 4] {
        assert!(
            center >= scores[&v],
            "PR({v}) = {} should be <= PR(2) = {center}",
            scores[&v]
        );
    }
}

#[test]
fn pagerank_normalizes_scores_into_unit_interval() {
    let g = linear_corpus();
    let result = PageRank::new("g").execute(&g).unwrap();
    for entry in result.inner().entries() {
        let s = entry.payload.score;
        assert!((0.0..=1.0).contains(&s), "score {s} out of range");
    }
}

#[test]
fn hits_sets_hub_authority_fields() {
    let g = star_corpus();
    let result = HITS::new("g").execute(&g).unwrap();
    let entry = result
        .inner()
        .entries()
        .iter()
        .find(|e| e.doc_id == 2)
        .expect("entry 2 missing");
    assert!(entry.payload.fields.contains_key("hub_score"));
    assert!(entry.payload.fields.contains_key("authority_score"));
    if let Some(Value::Float(a)) = entry.payload.fields.get("authority_score") {
        assert!((0.0..=1.0).contains(a));
    } else {
        panic!("authority_score not float");
    }
}

#[test]
fn betweenness_finds_bridge_vertex() {
    // Bridge: 1 - 2 - 3, with 2 the only path between 1 and 3.
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    for v in 1..=3 {
        g.add_vertex(Vertex::new(v, "n"), "g").unwrap();
    }
    g.add_edge(Edge::new(10, 1, 2, "e"), "g").unwrap();
    g.add_edge(Edge::new(11, 2, 3, "e"), "g").unwrap();
    let result = BetweennessCentrality::new("g").execute(&g).unwrap();
    let scores = score_map(&result);
    assert!(scores[&2] > scores[&1]);
    assert!(scores[&2] > scores[&3]);
}

#[test]
fn message_passing_propagates_initial_property() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    let mut alice = Vertex::new(1, "Person");
    alice.properties.insert("score".into(), Value::Int(10));
    let mut bob = Vertex::new(2, "Person");
    bob.properties.insert("score".into(), Value::Int(0));
    g.add_vertex(alice, "g").unwrap();
    g.add_vertex(bob, "g").unwrap();
    g.add_edge(Edge::new(10, 1, 2, "knows"), "g").unwrap();
    let result = MessagePassing::new("g")
        .property_name("score")
        .k_layers(1)
        .aggregation(AggregationKind::Sum)
        .execute(&g)
        .unwrap();
    let scores = score_map(&result);
    // Both vertices should have probability scores in (0, 1).
    assert!(scores[&1] > 0.0 && scores[&1] < 1.0);
    assert!(scores[&2] > 0.0 && scores[&2] < 1.0);
    // Bob's feature pulls in alice's 10, so its sigmoid score should be
    // higher than its initial sigmoid(0) = 0.5.
    assert!(scores[&2] > 0.5);
}

#[test]
fn message_passing_rejects_lossy_or_non_numeric_features() {
    for bad in [
        Value::Str("ten".into()),
        Value::Int((1_i64 << 53) + 1),
        Value::Float(f64::NAN),
    ] {
        let mut g = MemoryGraphStore::new();
        g.create_graph("g");
        let mut vertex = Vertex::new(1, "n");
        vertex.properties.insert("score".into(), bad);
        g.add_vertex(vertex, "g").unwrap();
        assert!(MessagePassing::new("g")
            .property_name("score")
            .execute(&g)
            .is_err());
    }
}

#[test]
fn message_passing_rejects_non_finite_accumulation_and_layer_exhaustion() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    for id in 1..=2 {
        let mut vertex = Vertex::new(id, "n");
        vertex
            .properties
            .insert("score".into(), Value::Float(f64::MAX));
        g.add_vertex(vertex, "g").unwrap();
    }
    g.add_edge(Edge::new(10, 1, 2, "e"), "g").unwrap();
    assert!(MessagePassing::new("g")
        .property_name("score")
        .k_layers(1)
        .aggregation(AggregationKind::Sum)
        .execute(&g)
        .is_err());
    assert!(MessagePassing::new("g")
        .k_layers(u32::MAX)
        .execute(&g)
        .is_err());
}

fn score_map(gpl: &uqa_graph::GraphPostingList) -> std::collections::BTreeMap<u64, f64> {
    gpl.inner()
        .entries()
        .iter()
        .map(|e| (e.doc_id, e.payload.score))
        .collect()
}
