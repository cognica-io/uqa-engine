//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SELECT * FROM rpq(expr, start, graph)` - Regular Path Query
//! evaluation as a SQL table function.

use uqa_core::{Edge, Vertex};
use uqa_engine::Engine;
use uqa_graph::GraphStore;

fn build_chain(eng: &Engine) {
    eng.create_graph("g");
    eng.graph_with_mut("g", |store| {
        store.add_vertex(Vertex::new(1, "P"), "g");
        store.add_vertex(Vertex::new(2, "P"), "g");
        store.add_vertex(Vertex::new(3, "P"), "g");
        store.add_vertex(Vertex::new(4, "P"), "g");
        store.add_edge(Edge::new(10, 1, 2, "manages"), "g");
        store.add_edge(Edge::new(11, 2, 3, "manages"), "g");
        store.add_edge(Edge::new(12, 3, 4, "manages"), "g");
    });
}

#[test]
fn rpq_kleene_star_returns_all_reachable() {
    let eng = Engine::new();
    build_chain(&eng);
    let r = eng
        .sql("SELECT * FROM rpq('manages*', 1, 'g')", &[])
        .unwrap();
    assert!(!r.rows.is_empty());
    // The Kleene star matches the empty path too, so the start vertex
    // is included alongside every descendant.
    let count = r.rows.len();
    assert!(count >= 4, "expected >= 4 reachable, got {count}");
}

#[test]
fn rpq_concat_two_hops() {
    let eng = Engine::new();
    build_chain(&eng);
    let r = eng
        .sql("SELECT * FROM rpq('manages/manages', 1, 'g')", &[])
        .unwrap();
    // exactly one endpoint (vertex 3) is reachable in two manages hops.
    assert_eq!(r.rows.len(), 1);
}

#[test]
fn rpq_from_function_uses_single_registered_graph() {
    let eng = Engine::new();
    build_chain(&eng);
    let r = eng
        .sql("SELECT * FROM rpq('manages/manages', 1)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 1);
}
