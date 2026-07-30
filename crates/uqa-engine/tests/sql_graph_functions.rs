//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tests for SQL `graph_*` functions exposed by the function registry.

use uqa_core::{Edge, Value, Vertex};
use uqa_engine::Engine;
use uqa_graph::GraphStore;

fn engine_with_simple_graph() -> Engine {
    let engine = Engine::new();
    // Plain table the graph functions can pull doc ids from.
    engine
        .sql(
            "CREATE TABLE seeds (id INTEGER PRIMARY KEY, status TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO seeds (id, status) VALUES \
             (1, 'indexed'), (2, 'draft'), (3, 'indexed'), (4, 'indexed')",
            &[],
        )
        .unwrap();
    // 1 -> 2 -> 3, 1 -> 4
    engine.create_graph("g").unwrap();
    engine
        .graph_with_mut("g", |store| {
            store.create_graph("g");
            for v in 1..=4 {
                store.add_vertex(Vertex::new(v, "n"), "g")?;
            }
            store.add_edge(Edge::new(10, 1, 2, "knows"), "g")?;
            store.add_edge(Edge::new(11, 2, 3, "knows"), "g")?;
            store.add_edge(Edge::new(12, 1, 4, "likes"), "g")?;
            Ok(())
        })
        .unwrap()
        .expect("graph exists");
    engine
}

#[test]
fn graph_traverse_returns_reachable_vertices() {
    let engine = engine_with_simple_graph();
    let result = engine
        .sql(
            "SELECT id FROM seeds WHERE graph_traverse('g', 1, 'knows', 2) ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn graph_traverse_combines_with_relational_filter() {
    let engine = engine_with_simple_graph();
    let result = engine
        .sql(
            "SELECT id FROM seeds \
             WHERE graph_traverse('g', 1, 'knows', 2) AND status = 'indexed' \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![1, 3]);
}

#[test]
fn graph_neighbors_returns_one_hop_targets() {
    let engine = engine_with_simple_graph();
    let result = engine
        .sql(
            "SELECT id FROM seeds WHERE graph_neighbors('g', 1, NULL, 'out') ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![2, 4]);
}

#[test]
fn graph_neighbors_combines_with_relational_filter() {
    let engine = engine_with_simple_graph();
    let result = engine
        .sql(
            "SELECT id FROM seeds \
             WHERE graph_neighbors('g', 1, NULL, 'out') AND status = 'indexed' \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![4]);
}

#[test]
fn graph_neighbors_label_filter() {
    let engine = engine_with_simple_graph();
    let result = engine
        .sql(
            "SELECT id FROM seeds WHERE graph_neighbors('g', 1, 'knows', 'out')",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![2]);
}

#[test]
fn graph_centrality_combines_with_relational_filter() {
    let engine = engine_with_simple_graph();
    let result = engine
        .sql(
            "SELECT id, _score FROM seeds \
             WHERE pagerank() AND status = 'indexed' \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![1, 3, 4]);
}

#[test]
fn graph_pagerank_scores_central_vertex_higher_in_star() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE seeds (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO seeds (id) VALUES (1), (2), (3), (4)", &[])
        .unwrap();
    engine.create_graph("star").unwrap();
    engine
        .graph_with_mut("star", |store| {
            store.create_graph("star");
            for v in 1..=4 {
                store.add_vertex(Vertex::new(v, "n"), "star")?;
            }
            store.add_edge(Edge::new(10, 1, 2, "e"), "star")?;
            store.add_edge(Edge::new(11, 3, 2, "e"), "star")?;
            store.add_edge(Edge::new(12, 4, 2, "e"), "star")?;
            Ok(())
        })
        .unwrap()
        .expect("graph exists");

    let result = engine
        .sql(
            "SELECT id, _score FROM seeds WHERE graph_pagerank('star') ORDER BY id",
            &[],
        )
        .unwrap();
    let mut scores: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
    for row in &result.rows {
        if let (Some(Value::Int(id)), Some(Value::Float(s))) = (row.get("id"), row.get("_score")) {
            scores.insert(*id, *s);
        }
    }
    let center = scores[&2];
    for v in [1i64, 3, 4] {
        assert!(
            center >= scores[&v],
            "PR(2)={center} should beat PR({v})={}",
            scores[&v]
        );
    }
}

#[test]
fn centrality_short_aliases_use_single_registered_graph() {
    let engine = engine_with_simple_graph();
    for function_name in ["pagerank", "hits", "betweenness"] {
        let sql =
            format!("SELECT _doc_id, _score FROM {function_name}() ORDER BY _score DESC LIMIT 4");
        let result = engine.sql(&sql, &[]).unwrap();
        assert!(!result.rows.is_empty(), "{function_name} returned no rows");
        assert!(
            result
                .rows
                .iter()
                .all(|row| matches!(row.get("_score"), Some(Value::Float(_)))),
            "{function_name} did not project _score"
        );
    }
}

#[test]
fn centrality_where_alias_uses_single_registered_graph() {
    let engine = engine_with_simple_graph();
    let result = engine
        .sql(
            "SELECT id, _score FROM seeds WHERE pagerank() ORDER BY _score DESC LIMIT 4",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    assert!(result
        .rows
        .iter()
        .all(|row| matches!(row.get("_score"), Some(Value::Float(_)))));
}

#[test]
fn graph_hits_and_betweenness_named_functions_return_rows() {
    let engine = engine_with_simple_graph();
    for function_name in ["graph_hits", "graph_betweenness"] {
        let sql = format!("SELECT id, _score FROM seeds WHERE {function_name}('g') ORDER BY id");
        let result = engine.sql(&sql, &[]).unwrap();
        assert!(!result.rows.is_empty(), "{function_name} returned no rows");
    }
}

#[test]
fn graph_traverse_unknown_graph_errors() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE seeds (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let err = engine
        .sql(
            "SELECT id FROM seeds WHERE graph_traverse('missing', 1, NULL, 1)",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("missing"), "{msg}");
}
