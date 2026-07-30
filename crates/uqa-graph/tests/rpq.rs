//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for the RPQ operator over `MemoryGraphStore`.

use std::sync::Arc;

use uqa_core::{DecimalValue, Edge, Value, Vertex, VertexId};
use uqa_graph::{
    parse_rpq, GraphStore, GraphStoreError, MemoryGraphStore, RegularPathExpr, RegularPathQuery,
    WeightedPathQuery,
};

fn corpus() -> MemoryGraphStore {
    // 1 -knows-> 2 -knows-> 3 -follows-> 4
    //                       |-likes-> 5
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    for vid in 1..=5 {
        g.add_vertex(Vertex::new(vid, "n"), "g").unwrap();
    }
    let mut first = Edge::new(10, 1, 2, "knows");
    first.properties.insert(
        "weight".into(),
        Value::Decimal(DecimalValue::parse("0.4").unwrap()),
    );
    g.add_edge(first, "g").unwrap();
    let mut second = Edge::new(11, 2, 3, "knows");
    second.properties.insert("weight".into(), Value::Float(0.7));
    g.add_edge(second, "g").unwrap();
    g.add_edge(Edge::new(12, 3, 4, "follows"), "g").unwrap();
    g.add_edge(Edge::new(13, 3, 5, "likes"), "g").unwrap();
    g
}

#[test]
fn rpq_single_label_one_hop() {
    let g = corpus();
    let result = RegularPathQuery::new(parse_rpq("knows").unwrap(), "g")
        .from_vertex(1)
        .execute(&g)
        .unwrap();
    let ids: Vec<VertexId> = result.inner().doc_ids().collect();
    assert_eq!(ids, vec![2]);
}

#[test]
fn rpq_concat_two_hops() {
    let g = corpus();
    let result = RegularPathQuery::new(parse_rpq("knows/knows").unwrap(), "g")
        .from_vertex(1)
        .execute(&g)
        .unwrap();
    let ids: Vec<VertexId> = result.inner().doc_ids().collect();
    assert_eq!(ids, vec![3]);
}

#[test]
fn rpq_alternation_branches() {
    let g = corpus();
    let result = RegularPathQuery::new(parse_rpq("follows|likes").unwrap(), "g")
        .from_vertex(3)
        .execute(&g)
        .unwrap();
    let mut ids: Vec<VertexId> = result.inner().doc_ids().collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![4, 5]);
}

#[test]
fn rpq_kleene_star_reaches_everything_via_knows() {
    let g = corpus();
    let result = RegularPathQuery::new(parse_rpq("knows*").unwrap(), "g")
        .from_vertex(1)
        .execute(&g)
        .unwrap();
    let mut ids: Vec<VertexId> = result.inner().doc_ids().collect();
    ids.sort_unstable();
    // knows* from 1: empty path -> 1, one hop -> 2, two hops -> 3.
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn accepting_empty_path_rejects_a_missing_start_vertex() {
    let g = corpus();
    assert!(matches!(
        RegularPathQuery::new(parse_rpq("knows*").unwrap(), "g")
            .from_vertex(999)
            .execute(&g),
        Err(GraphStoreError::InvalidQuery(message)) if message.contains("999")
    ));

    let mut weighted = WeightedPathQuery::new(
        parse_rpq("knows*").unwrap(),
        "g",
        "weight",
        Arc::new(|_| true),
    )
    .from_vertex(999);
    weighted.max_hops = 0;
    assert!(matches!(
        weighted.execute(&g),
        Err(GraphStoreError::InvalidQuery(message)) if message.contains("999")
    ));
}

#[test]
fn rpq_concat_alternation_and_star() {
    let g = corpus();
    // (knows*)/(follows|likes) from 1 should reach 4 and 5.
    let expr = RegularPathExpr::concat(
        RegularPathExpr::star(RegularPathExpr::label("knows")),
        RegularPathExpr::alt(
            RegularPathExpr::label("follows"),
            RegularPathExpr::label("likes"),
        ),
    );
    let result = RegularPathQuery::new(expr, "g")
        .from_vertex(1)
        .execute(&g)
        .unwrap();
    let mut ids: Vec<VertexId> = result.inner().doc_ids().collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![4, 5]);
}

#[test]
fn rpq_no_match_returns_empty() {
    let g = corpus();
    let result = RegularPathQuery::new(parse_rpq("missing").unwrap(), "g")
        .from_vertex(1)
        .execute(&g)
        .unwrap();
    assert!(result.inner().is_empty());
}

#[test]
fn rpq_unbounded_start_runs_from_every_vertex() {
    let g = corpus();
    let result = RegularPathQuery::new(parse_rpq("knows").unwrap(), "g")
        .execute(&g)
        .unwrap();
    let mut ids: Vec<VertexId> = result.inner().doc_ids().collect();
    ids.sort_unstable();
    // 1->2 and 2->3 reach 2 and 3.
    assert_eq!(ids, vec![2, 3]);
}

#[test]
fn rpq_execution_rejects_excessive_bounded_expansion() {
    let g = corpus();
    let path = RegularPathExpr::bounded(RegularPathExpr::label("knows"), 0, u32::MAX);
    let error = RegularPathQuery::new(path, "g")
        .from_vertex(1)
        .execute(&g)
        .unwrap_err();
    assert!(matches!(
        error,
        GraphStoreError::InvalidQuery(message) if message.contains("NFA")
    ));
}

#[test]
fn weighted_rpq_evaluates_the_real_path_predicate_and_preserves_path() {
    let g = corpus();
    let mut query = WeightedPathQuery::new(
        parse_rpq("knows/knows").unwrap(),
        "g",
        "weight",
        Arc::new(|weight| weight >= 1.0),
    )
    .from_vertex(1);
    query.max_hops = 2;
    let result = query.execute(&g).unwrap();

    assert_eq!(result.inner().doc_ids().collect::<Vec<_>>(), vec![3]);
    assert_eq!(
        result.inner().entries()[0]
            .payload
            .fields
            .get("_path_weight"),
        Some(&Value::Float(1.1))
    );
    let payload = result.get_graph_payload(3).unwrap();
    assert_eq!(payload.subgraph_vertices, vec![1, 2, 3]);
    assert_eq!(payload.subgraph_edges, vec![10, 11]);
}

#[test]
fn weighted_rpq_honours_the_explicit_walk_bound() {
    let g = corpus();
    let mut query = WeightedPathQuery::new(
        parse_rpq("knows/knows").unwrap(),
        "g",
        "weight",
        Arc::new(|_| true),
    )
    .from_vertex(1);
    query.max_hops = 1;
    assert!(query.execute(&g).unwrap().inner().is_empty());
}
