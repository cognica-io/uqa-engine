//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn drop_table_is_atomic_across_schema_and_owned_data() {
    let (dir, connection, engine) = persistent_engine();
    engine
        .sql(
            "CREATE TABLE kept_table (id INTEGER PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO kept_table VALUES (1, 'still here')", &[])
        .unwrap();

    fail_event(&connection, "_documents", "DELETE");
    assert!(engine.drop_table("kept_table").is_err());
    assert!(engine.has_table("kept_table").unwrap());
    assert_eq!(
        engine
            .get_document("kept_table", 1)
            .unwrap()
            .unwrap()
            .get("body"),
        Some(&uqa_core::Value::Str("still here".into()))
    );
    clear_failure(&connection);
    drop(engine);
    drop(connection);

    let reopened = Engine::open(&dir.path().join("catalog.db")).unwrap();
    assert!(reopened.has_table("kept_table").unwrap());
    assert_eq!(
        reopened
            .get_document("kept_table", 1)
            .unwrap()
            .unwrap()
            .get("body"),
        Some(&uqa_core::Value::Str("still here".into()))
    );
}

#[test]
fn unqualified_analyzer_assignment_uses_the_resolved_schema_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("analyzer-schema.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine.sql("CREATE SCHEMA app", &[]).unwrap();
        engine.set_search_path(vec!["app".to_string()]);
        engine
            .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
            .unwrap();
        engine
            .sql("CREATE INDEX docs_body_idx ON docs USING gin (body)", &[])
            .unwrap();
        engine
            .register_named_analyzer("strict", r#"{"tokenizer":"keyword"}"#)
            .unwrap();
        engine
            .set_table_field_analyzer("docs", "body", "strict", "both")
            .unwrap();
        assert_eq!(
            engine.table_field_analyzer("app.docs", "body").unwrap(),
            Some(("strict".to_string(), "both".to_string()))
        );
    }

    let reopened = Engine::open(&path).unwrap();
    assert_eq!(
        reopened.table_field_analyzer("app.docs", "body").unwrap(),
        Some(("strict".to_string(), "both".to_string()))
    );
}

#[test]
fn graph_mutation_and_drop_roll_back_on_catalog_failure() {
    let (_dir, connection, engine) = persistent_engine();
    engine.create_graph("g").unwrap();

    fail_event(&connection, "_named_graphs", "INSERT");
    assert!(engine
        .add_graph_vertex(Vertex::new(1, "Person"), "g")
        .is_err());
    assert_eq!(
        engine
            .graph_with("g", |graph| {
                graph.vertex_ids_in_graph("g").unwrap().len()
            })
            .unwrap()
            .unwrap(),
        0
    );
    clear_failure(&connection);

    fail_event(&connection, "_named_graphs", "DELETE");
    assert!(engine.drop_graph("g").is_err());
    assert!(engine.has_graph("g").unwrap());
}

#[test]
fn graph_edges_cannot_publish_dangling_endpoints() {
    let engine = Engine::new();
    engine.create_graph("g").unwrap();
    engine
        .add_graph_vertex(Vertex::new(1, "Person"), "g")
        .unwrap();
    let dangling = uqa_core::Edge::new(9, 1, 2, "KNOWS");
    assert!(engine.add_graph_edge(dangling, "g").is_err());
    assert_eq!(
        engine
            .graph_with("g", |graph| graph.edges_in_graph("g").unwrap().len())
            .unwrap(),
        Some(0)
    );
}

#[test]
fn analyze_does_not_publish_stats_when_catalog_write_fails() {
    let (_dir, connection, engine) = persistent_engine();
    engine.sql("CREATE TABLE stats_t (x INTEGER)", &[]).unwrap();
    engine.sql("INSERT INTO stats_t VALUES (1)", &[]).unwrap();
    engine.run_analyze(Some("stats_t")).unwrap();
    let before = engine.column_stats("stats_t").unwrap();

    fail_event(&connection, "_column_stats", "DELETE");
    assert!(engine.run_analyze(Some("stats_t")).is_err());
    let after = engine.column_stats("stats_t").unwrap();
    assert_eq!(after["x"].row_count, before["x"].row_count);
    assert_eq!(after["x"].distinct_count, before["x"].distinct_count);
}
