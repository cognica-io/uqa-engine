//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-level wiring for `apply_graph_delta` and the complete path-index
//! build, lookup, and drop lifecycle.

use tempfile::tempdir;
use uqa_core::{Edge, Vertex};
use uqa_engine::Engine;
use uqa_graph::{GraphDelta, GraphStore as _};

#[test]
fn apply_graph_delta_adds_then_removes_atomically() {
    let eng = Engine::new();
    eng.create_graph("g").unwrap();
    let mut delta = GraphDelta::new();
    delta.add_vertex(Vertex::new(1, "P"));
    delta.add_vertex(Vertex::new(2, "P"));
    delta.add_edge(Edge::new(10, 1, 2, "knows"));
    eng.apply_graph_delta("g", &delta).unwrap();
    let count = eng
        .graph_with("g", |store| store.vertices_in_graph("g").unwrap().len())
        .unwrap()
        .unwrap_or(0);
    assert_eq!(count, 2);

    let mut undo = GraphDelta::new();
    undo.remove_edge(10);
    undo.remove_vertex(1);
    eng.apply_graph_delta("g", &undo).unwrap();
    let edges = eng
        .graph_with("g", |store| store.edges_in_graph("g").unwrap().len())
        .unwrap()
        .unwrap_or(99);
    let verts = eng
        .graph_with("g", |store| store.vertices_in_graph("g").unwrap().len())
        .unwrap()
        .unwrap_or(99);
    assert_eq!(edges, 0);
    assert_eq!(verts, 1);
}

#[test]
fn build_path_index_then_get_then_drop() {
    let eng = Engine::new();
    eng.create_graph("g").unwrap();
    eng.add_graph_vertex(Vertex::new(1, "P"), "g").unwrap();
    eng.add_graph_vertex(Vertex::new(2, "P"), "g").unwrap();
    eng.add_graph_vertex(Vertex::new(3, "P"), "g").unwrap();
    eng.add_graph_edge(Edge::new(10, 1, 2, "manages"), "g")
        .unwrap();
    eng.add_graph_edge(Edge::new(11, 2, 3, "manages"), "g")
        .unwrap();

    eng.build_path_index(
        "manages_chain",
        "g",
        &[vec!["manages".to_string(), "manages".to_string()]],
    )
    .unwrap();
    let idx = eng
        .get_path_index("manages_chain", "g")
        .unwrap()
        .expect("index should be registered");
    let pairs = idx
        .lookup(&["manages".to_string(), "manages".to_string()])
        .expect("path-index query")
        .expect("indexed sequence missing");
    assert!(pairs.contains(&(1, 3)));

    assert!(eng.drop_path_index("manages_chain", "g").unwrap());
    assert!(eng.get_path_index("manages_chain", "g").unwrap().is_none());
}

#[test]
fn apply_graph_delta_invalidates_path_index() {
    let eng = Engine::new();
    eng.create_graph("g").unwrap();
    eng.add_graph_vertex(Vertex::new(1, "P"), "g").unwrap();
    eng.add_graph_vertex(Vertex::new(2, "P"), "g").unwrap();
    eng.add_graph_edge(Edge::new(10, 1, 2, "knows"), "g")
        .unwrap();
    eng.build_path_index("k", "g", &[vec!["knows".to_string()]])
        .unwrap();
    assert!(eng.get_path_index("k", "g").unwrap().is_some());

    let mut d = GraphDelta::new();
    d.add_vertex(Vertex::new(3, "P"));
    eng.apply_graph_delta("g", &d).unwrap();
    assert!(eng.get_path_index("k", "g").unwrap().is_none());
}

#[test]
fn graph_mutation_does_not_resurrect_a_stale_path_index_after_reopen() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("graph-path-index.db");
    {
        let engine = Engine::open(&database).unwrap();
        engine.create_graph("g").unwrap();
        engine.add_graph_vertex(Vertex::new(1, "P"), "g").unwrap();
        engine.add_graph_vertex(Vertex::new(2, "P"), "g").unwrap();
        engine
            .add_graph_edge(Edge::new(10, 1, 2, "knows"), "g")
            .unwrap();
        engine
            .build_path_index("k", "g", &[vec!["knows".to_string()]])
            .unwrap();
        assert!(engine.get_path_index("k", "g").unwrap().is_some());

        engine.add_graph_vertex(Vertex::new(3, "P"), "g").unwrap();
        assert!(engine.get_path_index("k", "g").unwrap().is_none());
    }

    let reopened = Engine::open(&database).unwrap();
    assert!(reopened.get_path_index("k", "g").unwrap().is_none());
    assert_eq!(reopened.list_path_indexes().unwrap().len(), 0);
    let vertices = reopened
        .graph_with("g", |store| store.vertex_ids_in_graph("g").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(vertices.into_iter().collect::<Vec<_>>(), vec![1, 2, 3]);
}

#[test]
fn graph_delta_with_spilled_endpoints_preserves_atomicity_across_providers() {
    use std::sync::Arc;
    use uqa_storage::mvcc::VersionedSessionOptions;
    use uqa_storage_sqlite::{ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider};

    let directory = tempdir().unwrap();
    let options = VersionedSessionOptions {
        retained_bytes: 1 << 20,
    };
    for provider in 0..3 {
        let engine = match provider {
            0 => {
                let connection =
                    ManagedConnection::open(&directory.path().join("native.db")).unwrap();
                connection.bind_native_records(options).unwrap();
                Engine::from_persistent_provider(Arc::new(SQLiteStorageProvider::new(connection)))
                    .unwrap()
            }
            1 => Engine::from_persistent_provider(Arc::new(
                SQLiteKeyValueStorage::open_with_options(
                    &directory.path().join("key-value.db"),
                    options,
                )
                .unwrap(),
            ))
            .unwrap(),
            _ => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open_with_options(
                    directory.path().join("store.redb"),
                    options,
                )
                .unwrap(),
            ))
            .unwrap(),
        };
        let mut delta = GraphDelta::new();
        // Canonical endpoint payloads alone exceed the session allowance, forcing private spills before the edges reuse those endpoints.
        for id in 1..=128 {
            let mut vertex = Vertex::new(id, "node");
            vertex
                .properties
                .insert("body".into(), uqa_core::Value::Str("x".repeat(16 << 10)));
            delta.add_vertex(vertex);
        }
        for id in 1..=384 {
            delta.add_edge(Edge::new(id, (id - 1) % 128 + 1, id % 128 + 1, "link"));
        }
        engine.apply_graph_delta("g", &delta).unwrap();
        let peer = engine.new_session().unwrap();
        let counts = || {
            peer.graph_with("g", |store| {
                (
                    store.vertex_ids_in_graph("g").unwrap().len(),
                    store.edge_id_page("g", None, 4096).unwrap().len(),
                )
            })
            .unwrap()
            .unwrap()
        };
        assert_eq!(counts(), (128, 384));
        let mut rejected = GraphDelta::new();
        rejected.add_vertex(Vertex::new(129, "node"));
        rejected.add_edge(Edge::new(385, 129, 999, "link"));
        assert!(engine.apply_graph_delta("g", &rejected).is_err());
        assert_eq!(counts(), (128, 384));
        assert!(peer
            .graph_with("g", |store| store.get_vertex(129).unwrap().is_none())
            .unwrap()
            .unwrap());
    }
}
