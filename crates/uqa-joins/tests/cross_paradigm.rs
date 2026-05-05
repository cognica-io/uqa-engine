//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for cross-paradigm join operators.

use std::collections::BTreeMap;

use uqa_core::{Edge, Payload, PostingEntry, Value, Vertex};
use uqa_graph::{GraphStore, MemoryGraphStore};
use uqa_joins::{
    CrossParadigmJoin, GraphJoin, HybridJoin, TextSimilarityJoin, VectorSimilarityJoin,
};

fn entry(id: u64, fields: BTreeMap<String, Value>, score: f64) -> PostingEntry {
    PostingEntry::new(
        id,
        Payload {
            positions: Vec::new(),
            score,
            fields,
        },
    )
}

fn fields(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

#[test]
fn text_similarity_join_emits_pairs_above_threshold() {
    let left = vec![
        entry(
            1,
            fields(&[("title", Value::Str("rust language".into()))]),
            0.0,
        ),
        entry(
            2,
            fields(&[("title", Value::Str("python ecosystem".into()))]),
            0.0,
        ),
    ];
    let right = vec![
        entry(
            10,
            fields(&[("title", Value::Str("rust language guide".into()))]),
            0.0,
        ),
        entry(
            11,
            fields(&[("title", Value::Str("python web".into()))]),
            0.0,
        ),
    ];
    let result = TextSimilarityJoin::new(&left, &right, "title", "title")
        .threshold(0.4)
        .execute();
    let pairs: Vec<(u64, u64)> = result
        .entries()
        .iter()
        .map(|e| (e.doc_ids[0], e.doc_ids[1]))
        .collect();
    assert!(pairs.contains(&(1, 10)));
}

#[test]
fn vector_similarity_join_filters_by_cosine() {
    let v_a = Value::List(vec![Value::Float(1.0), Value::Float(0.0)]);
    let v_b = Value::List(vec![Value::Float(0.9), Value::Float(0.1)]);
    let v_c = Value::List(vec![Value::Float(0.0), Value::Float(1.0)]);
    let left = vec![entry(1, fields(&[("emb", v_a.clone())]), 0.0)];
    let right = vec![
        entry(10, fields(&[("emb", v_b)]), 0.0),
        entry(11, fields(&[("emb", v_c)]), 0.0),
    ];
    let result = VectorSimilarityJoin::new(&left, &right, "emb", "emb")
        .threshold(0.5)
        .execute();
    let pairs: Vec<(u64, u64)> = result
        .entries()
        .iter()
        .map(|e| (e.doc_ids[0], e.doc_ids[1]))
        .collect();
    assert_eq!(pairs, vec![(1, 10)]);
}

#[test]
fn hybrid_join_filters_by_structured_field_and_cosine() {
    let v_a = Value::List(vec![Value::Float(1.0), Value::Float(0.0)]);
    let v_b = Value::List(vec![Value::Float(0.95), Value::Float(0.0)]);
    let left = vec![
        entry(
            1,
            fields(&[("dept", Value::Int(10)), ("emb", v_a.clone())]),
            0.0,
        ),
        entry(
            2,
            fields(&[("dept", Value::Int(20)), ("emb", v_a.clone())]),
            0.0,
        ),
    ];
    let right = vec![
        entry(
            10,
            fields(&[("dept", Value::Int(10)), ("emb", v_b.clone())]),
            0.0,
        ),
        entry(11, fields(&[("dept", Value::Int(30)), ("emb", v_b)]), 0.0),
    ];
    let result = HybridJoin::new(&left, &right, "dept", "emb")
        .threshold(0.5)
        .execute();
    let pairs: Vec<(u64, u64)> = result
        .entries()
        .iter()
        .map(|e| (e.doc_ids[0], e.doc_ids[1]))
        .collect();
    assert_eq!(pairs, vec![(1, 10)]);
}

#[test]
fn graph_join_emits_pairs_connected_by_edge() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    for v in 1..=3 {
        g.add_vertex(Vertex::new(v, "n"), "g");
    }
    g.add_edge(Edge::new(10, 1, 2, "knows"), "g");
    g.add_edge(Edge::new(11, 1, 3, "likes"), "g");

    let left = vec![entry(1, BTreeMap::new(), 0.5)];
    let right = vec![
        entry(2, BTreeMap::new(), 0.4),
        entry(3, BTreeMap::new(), 0.3),
    ];
    let result = GraphJoin::new(&left, &right, &g, "g")
        .label("knows")
        .execute();
    let pairs: Vec<(u64, u64)> = result
        .entries()
        .iter()
        .map(|e| (e.doc_ids[0], e.doc_ids[1]))
        .collect();
    assert_eq!(pairs, vec![(1, 2)]);
    if let Some(Value::Float(s)) = result.entries()[0].payload.fields.get("_score") {
        assert!((s - 0.9).abs() < 1e-9);
    } else {
        panic!("missing _score");
    }
}

#[test]
fn cross_paradigm_join_bridges_vertex_property_to_doc_field() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    let mut alice = Vertex::new(1, "Person");
    alice
        .properties
        .insert("email".into(), Value::Str("alice@example.com".into()));
    g.add_vertex(alice, "g");

    let left = vec![entry(1, BTreeMap::new(), 0.0)];
    let right = vec![
        entry(
            100,
            fields(&[("email", Value::Str("alice@example.com".into()))]),
            0.0,
        ),
        entry(
            101,
            fields(&[("email", Value::Str("bob@example.com".into()))]),
            0.0,
        ),
    ];
    let result = CrossParadigmJoin::new(&left, &right, &g, "email", "email").execute();
    let pairs: Vec<(u64, u64)> = result
        .entries()
        .iter()
        .map(|e| (e.doc_ids[0], e.doc_ids[1]))
        .collect();
    assert_eq!(pairs, vec![(1, 100)]);
}
