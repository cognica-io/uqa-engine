//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for `GraphDelta` + `VersionedGraphStore` and
//! `TemporalFilter` + `TemporalTraverse`.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use uqa_core::{Edge, Value, Vertex, VertexId};
use uqa_graph::{
    GraphDelta, GraphStore, GraphStoreError, MemoryGraphStore, TemporalFilter, TemporalTraverse,
    VersionedGraphStore,
};

#[test]
fn delta_records_affected_ids_and_labels() {
    let mut delta = GraphDelta::new();
    delta.add_vertex(Vertex::new(1, "Person"));
    delta.add_edge(Edge::new(10, 1, 2, "knows"));
    delta.remove_vertex(3);
    let vids: Vec<u64> = delta.affected_vertex_ids().into_iter().collect();
    assert_eq!(vids, vec![1, 2, 3]);
    let labels: Vec<String> = delta.affected_edge_labels().into_iter().collect();
    assert_eq!(labels, vec!["knows".to_string()]);
}

#[test]
fn versioned_store_applies_and_rolls_back() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    let mut versioned = VersionedGraphStore::new(&mut store, "g");

    let mut delta = GraphDelta::new();
    delta.add_vertex(Vertex::new(1, "Person"));
    delta.add_vertex(Vertex::new(2, "Person"));
    delta.add_edge(Edge::new(10, 1, 2, "knows"));
    let v1 = versioned.apply(delta).unwrap();
    assert_eq!(v1, 1);
    assert!(versioned.base().get_vertex(1).is_some());
    assert!(versioned.base().get_edge(10).is_some());

    let mut delta2 = GraphDelta::new();
    delta2.remove_edge(10);
    let v2 = versioned.apply(delta2).unwrap();
    assert_eq!(v2, 2);
    assert!(versioned.base().get_edge(10).is_none());

    versioned.rollback(1).unwrap();
    assert_eq!(versioned.version(), 1);
    // Edge restored.
    assert!(versioned.base().get_edge(10).is_some());

    versioned.rollback(0).unwrap();
    assert_eq!(versioned.version(), 0);
    assert!(versioned.base().get_vertex(1).is_none());
    assert!(versioned.base().get_edge(10).is_none());
}

#[test]
fn versioned_store_rollback_rejects_future_version() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    let mut versioned = VersionedGraphStore::new(&mut store, "g");
    let mut delta = GraphDelta::new();
    delta.add_vertex(Vertex::new(1, "Person"));
    versioned.apply(delta).unwrap();
    let result = versioned.rollback(5);
    assert!(result.is_err());
}

#[test]
fn versioned_store_invalidation_callback_fires_for_affected_labels() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    let mut versioned = VersionedGraphStore::new(&mut store, "g");

    let captured: Arc<Mutex<Vec<BTreeSet<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);
    versioned.on_invalidate(move |labels: &BTreeSet<String>| {
        captured_clone.lock().unwrap().push(labels.clone());
    });

    let mut delta = GraphDelta::new();
    delta.add_vertex(Vertex::new(1, "Person"));
    delta.add_vertex(Vertex::new(2, "Person"));
    delta.add_edge(Edge::new(10, 1, 2, "knows"));
    versioned.apply(delta).unwrap();

    {
        let logs = captured.lock().unwrap();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].contains("knows"));
    }

    let mut removal = GraphDelta::new();
    removal.remove_edge(10);
    versioned.apply(removal).unwrap();
    let logs = captured.lock().unwrap();
    assert_eq!(logs.len(), 2);
    assert!(logs[1].contains("knows"));
}

#[test]
fn temporal_filter_timestamp_inside_validity_window() {
    let mut props = std::collections::BTreeMap::new();
    props.insert("valid_from".into(), Value::Int(100));
    props.insert("valid_to".into(), Value::Int(200));
    let f = TemporalFilter::Timestamp(150.0);
    assert!(f.is_valid(&props).unwrap());
    let f2 = TemporalFilter::Timestamp(50.0);
    assert!(!f2.is_valid(&props).unwrap());
    let f3 = TemporalFilter::Timestamp(250.0);
    assert!(!f3.is_valid(&props).unwrap());
}

#[test]
fn temporal_filter_range_overlap_check() {
    let mut props = std::collections::BTreeMap::new();
    props.insert("valid_from".into(), Value::Int(100));
    props.insert("valid_to".into(), Value::Int(200));
    let overlapping = TemporalFilter::Range(150.0, 180.0);
    assert!(overlapping.is_valid(&props).unwrap());
    let disjoint = TemporalFilter::Range(300.0, 400.0);
    assert!(!disjoint.is_valid(&props).unwrap());
    let edge_touch = TemporalFilter::Range(200.0, 300.0);
    assert!(edge_touch.is_valid(&props).unwrap());
}

#[test]
fn temporal_filter_accepts_edge_without_temporal_props() {
    let props = std::collections::BTreeMap::new();
    let f = TemporalFilter::Timestamp(150.0);
    assert!(f.is_valid(&props).unwrap());
}

#[test]
fn temporal_traverse_filters_by_validity() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    for v in 1..=3 {
        g.add_vertex(Vertex::new(v, "n"), "g").unwrap();
    }
    let mut older = Edge::new(10, 1, 2, "knows");
    older.properties.insert("valid_from".into(), Value::Int(0));
    older.properties.insert("valid_to".into(), Value::Int(50));
    g.add_edge(older, "g").unwrap();
    let mut newer = Edge::new(11, 1, 3, "knows");
    newer
        .properties
        .insert("valid_from".into(), Value::Int(100));
    newer.properties.insert("valid_to".into(), Value::Int(200));
    g.add_edge(newer, "g").unwrap();

    // At t=150, only the newer edge is followed.
    let result = TemporalTraverse::new(1, "g")
        .label("knows")
        .max_hops(1)
        .filter(TemporalFilter::Timestamp(150.0))
        .execute(&g)
        .unwrap();
    let mut ids: Vec<VertexId> = result.inner().doc_ids().collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 3]);

    // At t=25, only the older edge is followed.
    let result2 = TemporalTraverse::new(1, "g")
        .label("knows")
        .max_hops(1)
        .filter(TemporalFilter::Timestamp(25.0))
        .execute(&g)
        .unwrap();
    let mut ids2: Vec<VertexId> = result2.inner().doc_ids().collect();
    ids2.sort_unstable();
    assert_eq!(ids2, vec![1, 2]);
}

#[test]
fn zero_hop_temporal_traverse_rejects_a_missing_start_vertex() {
    let mut graph = MemoryGraphStore::new();
    graph.create_graph("g");
    assert!(matches!(
        TemporalTraverse::new(999, "g")
            .max_hops(0)
            .execute(&graph),
        Err(GraphStoreError::InvalidQuery(message)) if message.contains("999")
    ));
}
