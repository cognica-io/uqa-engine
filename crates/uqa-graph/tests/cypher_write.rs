//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for the mutating `CypherWriter`.

use std::collections::BTreeMap;

use uqa_core::{Edge, Value, Vertex};
use uqa_graph::{
    cypher::{parse_cypher, CypherExecutor, CypherWriter},
    GraphStore, MemoryGraphStore,
};

fn fresh() -> MemoryGraphStore {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    g
}

fn run_read(store: &MemoryGraphStore, src: &str) -> (Vec<String>, Vec<BTreeMap<String, Value>>) {
    let exec = CypherExecutor::new(store, "g");
    let query = parse_cypher(src).unwrap();
    exec.execute(&query).unwrap()
}

fn run_write(
    store: &mut MemoryGraphStore,
    src: &str,
) -> (Vec<String>, Vec<BTreeMap<String, Value>>) {
    let mut writer = CypherWriter::new(store, "g");
    let query = parse_cypher(src).unwrap();
    writer.execute(&query).unwrap()
}

#[test]
fn create_node_and_read_back() {
    let mut g = fresh();
    run_write(&mut g, "CREATE (n:Person {name: 'alice', age: 30})");
    let (_, rows) = run_read(&g, "MATCH (n:Person) RETURN n.name AS name, n.age AS age");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("name"), Some(&Value::Str("alice".into())));
    assert_eq!(rows[0].get("age"), Some(&Value::Int(30)));
}

#[test]
fn create_relationship_between_existing_nodes() {
    let mut g = fresh();
    g.add_vertex(Vertex::new(1, "Person"), "g");
    g.add_vertex(Vertex::new(2, "Person"), "g");
    run_write(
        &mut g,
        "MATCH (a:Person), (b:Person) WHERE id(a) = 1 AND id(b) = 2 \
         CREATE (a)-[r:KNOWS {since: 2024}]->(b)",
    );
    let (_, rows) = run_read(
        &g,
        "MATCH (a:Person)-[r:KNOWS]->(b:Person) RETURN r.since AS s",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("s"), Some(&Value::Int(2024)));
}

#[test]
fn set_property_assign_and_update() {
    let mut g = fresh();
    let mut v = Vertex::new(1, "Person");
    v.properties.insert("age".into(), Value::Int(30));
    g.add_vertex(v, "g");
    run_write(
        &mut g,
        "MATCH (n:Person) WHERE id(n) = 1 SET n.age = 31, n.name = 'alice'",
    );
    let (_, rows) = run_read(&g, "MATCH (n:Person) RETURN n.name AS name, n.age AS age");
    assert_eq!(rows[0].get("age"), Some(&Value::Int(31)));
    assert_eq!(rows[0].get("name"), Some(&Value::Str("alice".into())));
}

#[test]
fn delete_vertex_with_no_edges() {
    let mut g = fresh();
    g.add_vertex(Vertex::new(1, "Person"), "g");
    g.add_vertex(Vertex::new(2, "Person"), "g");
    run_write(&mut g, "MATCH (n:Person) WHERE id(n) = 1 DELETE n");
    let (_, rows) = run_read(&g, "MATCH (n:Person) RETURN n.name AS name");
    assert_eq!(rows.len(), 1);
}

#[test]
fn delete_with_incident_edges_fails_without_detach() {
    let mut g = fresh();
    g.add_vertex(Vertex::new(1, "Person"), "g");
    g.add_vertex(Vertex::new(2, "Person"), "g");
    g.add_edge(Edge::new(10, 1, 2, "KNOWS"), "g");
    let mut writer = CypherWriter::new(&mut g, "g");
    let query = parse_cypher("MATCH (n:Person) WHERE id(n) = 1 DELETE n").unwrap();
    let result = writer.execute(&query);
    assert!(result.is_err(), "expected error: {result:?}");
}

#[test]
fn detach_delete_removes_vertex_and_edges() {
    let mut g = fresh();
    g.add_vertex(Vertex::new(1, "Person"), "g");
    g.add_vertex(Vertex::new(2, "Person"), "g");
    g.add_edge(Edge::new(10, 1, 2, "KNOWS"), "g");
    run_write(&mut g, "MATCH (n:Person) WHERE id(n) = 1 DETACH DELETE n");
    let (_, vrows) = run_read(&g, "MATCH (n:Person) RETURN id(n) AS i");
    assert_eq!(vrows.len(), 1);
    let (_, erows) = run_read(&g, "MATCH (a)-[r:KNOWS]->(b) RETURN id(r) AS i");
    assert!(erows.is_empty());
}

#[test]
fn merge_creates_when_missing_then_matches() {
    let mut g = fresh();
    run_write(&mut g, "MERGE (n:Person {name: 'alice'})");
    let (_, rows1) = run_read(&g, "MATCH (n:Person) RETURN n.name AS name");
    assert_eq!(rows1.len(), 1);
    // Second MERGE on the same identity should not create a duplicate.
    run_write(&mut g, "MERGE (n:Person {name: 'alice'})");
    let (_, rows2) = run_read(&g, "MATCH (n:Person) RETURN n.name AS name");
    assert_eq!(rows2.len(), 1);
}

#[test]
fn merge_on_create_set_and_on_match_set() {
    let mut g = fresh();
    run_write(
        &mut g,
        "MERGE (n:Person {name: 'alice'}) ON CREATE SET n.flag = 'new'",
    );
    let (_, rows) = run_read(&g, "MATCH (n:Person) RETURN n.flag AS f");
    assert_eq!(rows[0].get("f"), Some(&Value::Str("new".into())));
    run_write(
        &mut g,
        "MERGE (n:Person {name: 'alice'}) ON MATCH SET n.flag = 'seen'",
    );
    let (_, rows2) = run_read(&g, "MATCH (n:Person) RETURN n.flag AS f");
    assert_eq!(rows2[0].get("f"), Some(&Value::Str("seen".into())));
}

#[test]
fn unwind_creates_one_per_element() {
    let mut g = fresh();
    run_write(&mut g, "UNWIND [1, 2, 3] AS x CREATE (n:Number {value: x})");
    let (_, rows) = run_read(&g, "MATCH (n:Number) RETURN n.value AS v ORDER BY n.value");
    let values: Vec<i64> = rows
        .iter()
        .filter_map(|r| match r.get("v") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(values, vec![1, 2, 3]);
}
