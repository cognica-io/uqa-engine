//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn graph_namespaces_and_schemas_share_one_name_space() {
    let engine = Engine::new();
    exec(&engine, "SELECT create_graph('shared_ns')");
    let duplicate = engine.sql("CREATE SCHEMA shared_ns", &[]).unwrap_err();
    assert!(
        duplicate.to_string().contains("already exists"),
        "{duplicate}"
    );
    assert!(exec(&engine, "CREATE SCHEMA IF NOT EXISTS shared_ns")
        .rows
        .is_empty());
    assert_age_error(
        &engine,
        "DROP SCHEMA shared_ns",
        "2BP01",
        "cannot drop schema shared_ns because other objects depend on it",
    );
    assert!(engine.has_graph("shared_ns").unwrap());
    exec(&engine, "DROP SCHEMA shared_ns CASCADE");
    assert!(!engine.has_graph("shared_ns").unwrap());
    let reserved = engine.sql("CREATE SCHEMA ag_catalog", &[]).unwrap_err();
    assert!(reserved.to_string().contains("reserved"), "{reserved}");
    let protected = engine.sql("DROP SCHEMA ag_catalog", &[]).unwrap_err();
    assert!(
        protected.to_string().contains("cannot be dropped"),
        "{protected}"
    );
    // IF EXISTS keeps skipping unknown names even with CASCADE.
    assert!(exec(&engine, "DROP SCHEMA IF EXISTS never_created CASCADE")
        .rows
        .is_empty());
    exec(&engine, "SELECT create_graph('shared_again')");
    exec(
        &engine,
        "DROP SCHEMA IF EXISTS shared_again, never_created CASCADE",
    );
    assert!(!engine.has_graph("shared_again").unwrap());
    // Virtual and graph namespaces are visible to the session schema functions.
    exec(&engine, "SELECT create_graph('visible_ns')");
    exec(
        &engine,
        "SET search_path = ag_catalog, visible_ns, \"$user\", public",
    );
    assert_eq!(
        scalar(&engine, "SELECT current_schema()"),
        Value::Str("ag_catalog".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT current_schemas(false)::text"),
        Value::Str("{ag_catalog,visible_ns,public}".into())
    );
}

#[test]
fn labels_and_renames_persist_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("age-catalog.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(&engine, "SELECT create_graph('durable')");
        exec(&engine, "SELECT create_vlabel('durable', 'Empty')");
        exec(&engine, "SELECT create_elabel('durable', 'LINKS')");
        exec(
            &engine,
            "SELECT * FROM cypher('durable', $$
                 CREATE (:Person {name: 'ada'})-[:LINKS]->(:Person {name: 'bob'})
             $$) AS (ignored agtype)",
        );
        exec(
            &engine,
            "SELECT alter_graph('durable', 'RENAME', 'durable2')",
        );
    }
    let engine = Engine::open(&database).unwrap();
    assert!(!engine.has_graph("durable").unwrap());
    let labels = engine.list_graph_labels("durable2").unwrap().unwrap();
    let shaped: Vec<(String, u32, LabelKind)> = labels
        .iter()
        .map(|label| (label.name.clone(), label.id, label.kind))
        .collect();
    assert_eq!(
        shaped,
        vec![
            ("_ag_label_vertex".into(), 1, LabelKind::Vertex),
            ("_ag_label_edge".into(), 2, LabelKind::Edge),
            ("Empty".into(), 3, LabelKind::Vertex),
            ("LINKS".into(), 4, LabelKind::Edge),
            ("Person".into(), 5, LabelKind::Vertex),
        ]
    );
    // The empty label survived without any entity, and new entities keep
    // allocating from the persisted label ids.
    exec(
        &engine,
        "SELECT * FROM cypher('durable2', $$ CREATE (:Empty {n: 1}) $$) AS (ignored agtype)",
    );
    let ids = exec(
        &engine,
        "SELECT * FROM cypher('durable2', $$ MATCH (n:Empty) RETURN id(n) $$) AS (id bigint)",
    );
    assert_eq!(ids.rows.len(), 1);
    assert_eq!(int(ids.rows[0].get("id")), (3_i64 << 48) | 1);
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM ag_catalog.ag_label WHERE kind = 'e'"
        ),
        Value::Int(2)
    );
}

#[test]
fn dropped_vertex_label_and_dangling_edges_persist_across_reopen() {
    age_default_label_drop::dropped_vertex_label_and_dangling_edges_persist_across_reopen();
}

#[test]
fn engine_api_exposes_the_label_catalog() {
    let engine = Engine::new();
    assert!(engine.list_graph_labels("missing").unwrap().is_none());
    engine.create_graph("api").unwrap();
    assert!(engine
        .create_graph_label("api", "Person", LabelKind::Vertex)
        .unwrap());
    assert!(!engine
        .create_graph_label("api", "Person", LabelKind::Edge)
        .unwrap());
    assert!(engine
        .create_graph_label("missing", "Person", LabelKind::Vertex)
        .is_err());
    assert!(engine.drop_graph_label("api", "Person").unwrap());
    assert!(!engine.drop_graph_label("api", "Person").unwrap());
    assert!(engine.rename_graph("api", "api2").unwrap());
    assert!(!engine.rename_graph("api", "api2").unwrap());
    engine.create_graph("api3").unwrap();
    assert!(engine.rename_graph("api2", "api3").is_err());
    assert_eq!(engine.list_graphs().unwrap(), vec!["api2", "api3"]);
}
