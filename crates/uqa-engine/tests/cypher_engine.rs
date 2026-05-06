//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end test for `Engine::run_cypher` covering CREATE / MERGE /
//! DELETE / SET against a named graph.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn create_then_match_returns_inserted_node() {
    let eng = Engine::new();
    let (_, _) = eng
        .run_cypher(
            "g",
            "CREATE (n:Person {name: 'Alice', age: 30})",
            BTreeMap::new(),
        )
        .unwrap();
    let (cols, rows) = eng
        .run_cypher(
            "g",
            "MATCH (n:Person {name: 'Alice'}) RETURN n.name AS name, n.age AS age",
            BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(cols, vec!["name".to_string(), "age".to_string()]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("name"), Some(&Value::Str("Alice".into())));
    assert_eq!(rows[0].get("age"), Some(&Value::Int(30)));
}

#[test]
fn merge_creates_when_missing_then_matches_on_repeat() {
    let eng = Engine::new();
    let (_, _) = eng
        .run_cypher(
            "g",
            "MERGE (n:Tag {name: 'rust'}) ON CREATE SET n.created = true",
            BTreeMap::new(),
        )
        .unwrap();
    let (_, rows) = eng
        .run_cypher(
            "g",
            "MATCH (n:Tag) RETURN n.name AS name, n.created AS created",
            BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("name"), Some(&Value::Str("rust".into())));
    assert_eq!(rows[0].get("created"), Some(&Value::Bool(true)));

    // Second MERGE matches the existing node — must not create a duplicate.
    let (_, _) = eng
        .run_cypher(
            "g",
            "MERGE (n:Tag {name: 'rust'}) ON MATCH SET n.touched = true",
            BTreeMap::new(),
        )
        .unwrap();
    let (_, rows) = eng
        .run_cypher(
            "g",
            "MATCH (n:Tag) RETURN n.name AS name, n.touched AS touched",
            BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("touched"), Some(&Value::Bool(true)));
}

#[test]
fn delete_removes_node() {
    let eng = Engine::new();
    eng.run_cypher(
        "g",
        "CREATE (n:Tag {name: 'a'}), (m:Tag {name: 'b'})",
        BTreeMap::new(),
    )
    .unwrap();
    eng.run_cypher("g", "MATCH (n:Tag {name: 'a'}) DELETE n", BTreeMap::new())
        .unwrap();
    let (_, rows) = eng
        .run_cypher("g", "MATCH (n:Tag) RETURN n.name AS name", BTreeMap::new())
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("name"), Some(&Value::Str("b".into())));
}
