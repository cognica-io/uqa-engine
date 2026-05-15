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
fn apache_age_style_cypher_table_function_round_trips() {
    let eng = Engine::new();
    eng.sql("SELECT create_graph('g') AS ok", &[]).unwrap();
    let created = eng
        .sql(
            "SELECT * FROM ag_catalog.cypher('g', $$
                CREATE (n:Person {name: 'Alice', age: 30})
            $$) AS (ignored agtype)",
            &[],
        )
        .unwrap();
    assert_eq!(created.columns, vec!["ignored".to_string()]);
    assert!(created.rows.is_empty());

    let result = eng
        .sql(
            "SELECT * FROM cypher('g', $$
                MATCH (n:Person {name: 'Alice'})
                RETURN n.name, n.age
            $$) AS (name agtype, age agtype)",
            &[],
        )
        .unwrap();
    assert_eq!(result.columns, vec!["name".to_string(), "age".to_string()]);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(
        result.rows[0].get("name"),
        Some(&Value::Str("Alice".into()))
    );
    assert_eq!(result.rows[0].get("age"), Some(&Value::Int(30)));
}

#[test]
fn apache_age_style_cypher_prepared_parameters() {
    let eng = Engine::new();
    eng.sql("SELECT create_graph('g') AS ok", &[]).unwrap();
    eng.sql(
        "SELECT * FROM cypher('g', $$
            CREATE (n:Person {name: 'Alice', age: 30})
        $$) AS (ignored agtype)",
        &[],
    )
    .unwrap();
    eng.sql(
        "PREPARE find_person AS
         SELECT * FROM cypher('g', $$
             MATCH (n:Person)
             WHERE n.name = $name
             RETURN n.age
         $$, $1) AS (age agtype)",
        &[],
    )
    .unwrap();

    let result = eng
        .sql("EXECUTE find_person ('{\"name\":\"Alice\"}')", &[])
        .unwrap();
    assert_eq!(result.columns, vec!["age".to_string()]);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("age"), Some(&Value::Int(30)));
}

#[test]
fn apache_age_style_cypher_requires_record_definition() {
    let eng = Engine::new();
    eng.sql("SELECT create_graph('g') AS ok", &[]).unwrap();
    let err = eng
        .sql("SELECT * FROM cypher('g', $$ RETURN 1 $$)", &[])
        .unwrap_err()
        .to_string();
    assert!(err.contains("record definition"), "{err}");
}

#[test]
fn apache_age_style_cypher_requires_existing_graph() {
    let eng = Engine::new();
    let err = eng
        .sql(
            "SELECT * FROM cypher('missing', $$ RETURN 1 $$) AS (v agtype)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not exist"), "{err}");
}

#[test]
fn apache_age_style_cypher_rejects_literal_parameter_map() {
    let eng = Engine::new();
    eng.sql("SELECT create_graph('g') AS ok", &[]).unwrap();
    let err = eng
        .sql(
            "SELECT * FROM cypher('g', $$ RETURN $name $$, '{\"name\":\"Alice\"}')
             AS (name agtype)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("SQL parameter"), "{err}");
}

#[test]
fn apache_age_create_graph_aliases_existing_graph_functions() {
    let eng = Engine::new();
    let created = eng.sql("SELECT create_graph('social') AS ok", &[]).unwrap();
    assert_eq!(created.rows[0].get("ok"), Some(&Value::Bool(true)));
    let dropped = eng.sql("SELECT drop_graph('social') AS ok", &[]).unwrap();
    assert_eq!(dropped.rows[0].get("ok"), Some(&Value::Bool(true)));
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
