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
    eng.sql("SELECT create_graph('demo') AS ok", &[]).unwrap();
    let created = eng
        .sql(
            "SELECT * FROM ag_catalog.cypher('demo', $$
                CREATE (n:Person {name: 'Alice', age: 30})
            $$) AS (ignored agtype)",
            &[],
        )
        .unwrap();
    assert_eq!(created.columns, vec!["ignored".to_string()]);
    assert!(created.rows.is_empty());

    // agtype columns carry canonical AGE text: strings render
    // JSON-quoted, integers bare.
    let result = eng
        .sql(
            "SELECT * FROM cypher('demo', $$
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
        Some(&Value::Str("\"Alice\"".into()))
    );
    assert_eq!(result.rows[0].get("age"), Some(&Value::Str("30".into())));

    // Scalar column types coerce like AGE casts.
    let typed = eng
        .sql(
            "SELECT * FROM cypher('demo', $$
                MATCH (n:Person {name: 'Alice'})
                RETURN n.name, n.age
            $$) AS (name text, age int)",
            &[],
        )
        .unwrap();
    assert_eq!(typed.rows[0].get("name"), Some(&Value::Str("Alice".into())));
    assert_eq!(typed.rows[0].get("age"), Some(&Value::Int(30)));
}

#[test]
fn apache_age_style_cypher_prepared_parameters() {
    let eng = Engine::new();
    eng.sql("SELECT create_graph('demo') AS ok", &[]).unwrap();
    eng.sql(
        "SELECT * FROM cypher('demo', $$
            CREATE (n:Person {name: 'Alice', age: 30})
        $$) AS (ignored agtype)",
        &[],
    )
    .unwrap();
    eng.sql(
        "PREPARE find_person AS
         SELECT * FROM cypher('demo', $$
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
    assert_eq!(result.rows[0].get("age"), Some(&Value::Str("30".into())));
}

#[test]
fn apache_age_style_cypher_requires_record_definition() {
    let eng = Engine::new();
    eng.sql("SELECT create_graph('demo') AS ok", &[]).unwrap();
    let err = eng
        .sql("SELECT * FROM cypher('demo', $$ RETURN 1 $$)", &[])
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
    eng.sql("SELECT create_graph('demo') AS ok", &[]).unwrap();
    let err = eng
        .sql(
            "SELECT * FROM cypher('demo', $$ RETURN $name $$, '{\"name\":\"Alice\"}')
             AS (name agtype)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("SQL parameter"), "{err}");
}

#[test]
fn apache_age_create_graph_returns_void_and_validates_name() {
    let eng = Engine::new();
    // AGE returns void (SQL NULL) from create_graph / drop_graph.
    let created = eng.sql("SELECT create_graph('social') AS ok", &[]).unwrap();
    assert_eq!(created.rows[0].get("ok"), Some(&Value::Null));
    // Duplicate names are rejected.
    let err = eng
        .sql("SELECT create_graph('social') AS ok", &[])
        .unwrap_err()
        .to_string();
    assert!(err.contains("already exists"), "{err}");
    let dropped = eng
        .sql("SELECT drop_graph('social', true) AS ok", &[])
        .unwrap();
    assert_eq!(dropped.rows[0].get("ok"), Some(&Value::Null));

    // Name validation: >= 3 chars, first char letter or underscore.
    for invalid in ["ab", "a1", "1ab"] {
        let err = eng
            .sql(&format!("SELECT create_graph('{invalid}')"), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("graph name is invalid"), "{invalid}: {err}");
    }
    for valid in ["abc", "ab1", "_ab", "AB2"] {
        eng.sql(&format!("SELECT create_graph('{valid}')"), &[])
            .unwrap_or_else(|e| panic!("{valid}: {e}"));
        eng.sql(&format!("SELECT drop_graph('{valid}', true)"), &[])
            .unwrap();
    }
}

#[test]
fn apache_age_drop_graph_requires_cascade() {
    let eng = Engine::new();
    eng.sql("SELECT create_graph('social') AS ok", &[]).unwrap();
    eng.sql(
        "SELECT * FROM cypher('social', $$
            CREATE (:Person {name: 'Alice'})
        $$) AS (ignored agtype)",
        &[],
    )
    .unwrap();

    // AGE's drop_graph without cascade maps to DROP SCHEMA RESTRICT,
    // which always fails because the label tables depend on it.
    let err = eng
        .sql("SELECT drop_graph('social', false) AS ok", &[])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("cannot drop schema social because other objects depend on it"),
        "{err}"
    );
    let err = eng
        .sql("SELECT drop_graph('social') AS ok", &[])
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot drop schema"), "{err}");

    let dropped = eng
        .sql("SELECT drop_graph('social', true) AS ok", &[])
        .unwrap();
    assert_eq!(dropped.rows[0].get("ok"), Some(&Value::Null));
    assert!(!eng.has_graph("social"));

    // Dropping a missing graph reports it does not exist.
    let err = eng
        .sql("SELECT drop_graph('social', true) AS ok", &[])
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not exist"), "{err}");
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
