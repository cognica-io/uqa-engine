//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-level wiring for `apply_graph_delta` and the
//! `build/drop/get_path_index` lifecycle. Mirrors the canonical UQA implementation's
//! `Engine.apply_graph_delta` / `Engine.build_path_index`.

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
    assert!(reopened.list_path_indexes().unwrap().is_empty());
    let vertices = reopened
        .graph_with("g", |store| store.vertex_ids_in_graph("g").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(vertices.into_iter().collect::<Vec<_>>(), vec![1, 2, 3]);
}
