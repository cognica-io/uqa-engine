//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{assert_age_error, exec, scalar, strings};
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_graph::GraphStore as _;

pub(super) fn drop_label_removes_entities_and_preserves_incident_edge_rows(engine: &Engine) {
    assert_eq!(
        scalar(engine, "SELECT drop_label('labels', 'KNOWS')"),
        Value::Null
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT * FROM cypher('labels', $$ MATCH ()-[r]->() RETURN count(r) $$) AS (c bigint)"
        ),
        Value::Int(1)
    );
    exec(engine, "SELECT create_elabel('labels', 'KNOWS')");
    exec(
        engine,
        "SELECT * FROM cypher('labels', $$
             MATCH (a:Person {name: 'ada'}), (b:Person {name: 'bob'})
             CREATE (a)-[:KNOWS]->(b)
         $$) AS (ignored agtype)",
    );
    assert_eq!(
        scalar(engine, "SELECT drop_label('labels', 'Person')"),
        Value::Null
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT * FROM cypher('labels', $$ MATCH (n) RETURN count(n) $$) AS (c bigint)"
        ),
        Value::Int(1)
    );
    let dangling_edges = engine
        .graph_with("labels", |store| store.edges_in_graph("labels").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(dangling_edges.len(), 2);
    assert!(dangling_edges
        .iter()
        .all(|edge| { (edge.source_id >> 48) == 3 && (edge.target_id >> 48) == 3 }));
    assert_age_error(
        engine,
        "SELECT drop_label('labels', '_ag_label_edge')",
        "2BP01",
        "cannot drop table labels._ag_label_edge because other objects depend on it",
    );
    assert_eq!(
        strings(
            engine,
            "SELECT name FROM ag_catalog.ag_label ORDER BY id",
            "name"
        ),
        vec!["_ag_label_vertex", "_ag_label_edge", "KNOWS"]
    );
}

pub(super) fn default_label_drop_follows_age_restrict_and_broken_graph_lifecycle(engine: &Engine) {
    drop_nonempty_default_vertex(engine);
    drop_default_edge_and_keep_vertex_kind_usable(engine);
    default_label_dependencies_are_restrictive(engine);
    drop_graph_restrict_succeeds_after_all_labels_are_gone(engine);
    for graph in [
        "default_nonempty",
        "default_edge_missing",
        "default_vertex_child",
        "default_view_renamed",
        "default_drop_restrict",
    ] {
        exec(engine, &format!("SELECT drop_graph('{graph}', true)"));
    }
}

fn drop_nonempty_default_vertex(engine: &Engine) {
    exec(engine, "SELECT create_graph('default_nonempty')");
    exec(
        engine,
        "SELECT * FROM cypher('default_nonempty', $$
             CREATE (a:_ag_label_vertex)-[:_ag_label_edge]->(b:_ag_label_vertex)
         $$) AS (ignored agtype)",
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT drop_label('default_nonempty', '_ag_label_vertex')"
        ),
        Value::Null
    );
    assert_eq!(
        strings(
            engine,
            "SELECT name FROM ag_catalog.ag_label WHERE graph = (SELECT graphid FROM ag_catalog.ag_graph WHERE name = 'default_nonempty') ORDER BY id",
            "name"
        ),
        vec!["_ag_label_edge"]
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace WHERE n.nspname = 'default_nonempty' AND c.relname IN ('_ag_label_vertex', '_ag_label_vertex_id_seq')"
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar(engine, "SELECT graph_exists('default_nonempty')"),
        Value::Str("true".into())
    );
    let graph_state = engine
        .graph_with("default_nonempty", |store| {
            (
                store.vertex_ids_in_graph("default_nonempty").unwrap(),
                store.edges_in_graph("default_nonempty").unwrap(),
            )
        })
        .unwrap()
        .unwrap();
    assert!(graph_state.0.is_empty());
    assert_eq!(graph_state.1.len(), 1);
    assert_eq!(graph_state.1[0].source_id >> 48, 1);
    assert_eq!(graph_state.1[0].target_id >> 48, 1);
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) FROM default_nonempty._ag_label_edge"
        ),
        Value::Int(1)
    );
    assert_age_error(
        engine,
        "SELECT count(*) FROM default_nonempty._ag_label_vertex",
        "42P01",
        "relation \"default_nonempty._ag_label_vertex\" does not exist",
    );
    missing_default_vertex_operations_fail_safely(engine);
    assert_eq!(
        scalar(
            engine,
            "SELECT create_elabel('default_nonempty', 'SURVIVING_KIND')"
        ),
        Value::Null
    );
}

fn missing_default_vertex_operations_fail_safely(engine: &Engine) {
    assert_age_error(
        engine,
        "SELECT drop_label('default_nonempty', '_ag_label_vertex')",
        "42P01",
        "label \"_ag_label_vertex\" does not exist",
    );
    assert_age_error(
        engine,
        "SELECT create_vlabel('default_nonempty', 'Person')",
        "42P01",
        "relation \"default_nonempty._ag_label_vertex\" does not exist",
    );
    for query in ["MATCH (n) RETURN count(n)", "RETURN exists((n))"] {
        assert_age_error(
            engine,
            &format!("SELECT * FROM cypher('default_nonempty', $$ {query} $$) AS (result agtype)"),
            "42P01",
            "relation \"default_nonempty._ag_label_vertex\" does not exist",
        );
    }
}

fn drop_default_edge_and_keep_vertex_kind_usable(engine: &Engine) {
    exec(engine, "SELECT create_graph('default_edge_missing')");
    exec(
        engine,
        "SELECT drop_label('default_edge_missing', '_ag_label_edge')",
    );
    exec(
        engine,
        "SELECT * FROM cypher('default_edge_missing', $$ CREATE (:Person) $$) AS (ignored agtype)",
    );
    for query in [
        "MATCH ()-[r]->() RETURN count(r)",
        "RETURN exists((a)-[r]->(b))",
    ] {
        assert_age_error(
            engine,
            &format!(
                "SELECT * FROM cypher('default_edge_missing', $$ {query} $$) AS (result agtype)"
            ),
            "42P01",
            "relation \"default_edge_missing._ag_label_edge\" does not exist",
        );
    }
}

fn default_label_dependencies_are_restrictive(engine: &Engine) {
    exec(engine, "SELECT create_graph('default_vertex_child')");
    exec(
        engine,
        "SELECT create_vlabel('default_vertex_child', 'Person')",
    );
    assert_age_error(
        engine,
        "SELECT drop_label('default_vertex_child', '_ag_label_vertex')",
        "2BP01",
        "cannot drop table default_vertex_child._ag_label_vertex because other objects depend on it",
    );
    exec(engine, "SELECT create_graph('default_view_dep')");
    exec(
        engine,
        "CREATE VIEW default_label_view AS SELECT id FROM default_view_dep._ag_label_vertex",
    );
    assert_age_error(
        engine,
        "SELECT drop_label('default_view_dep', '_ag_label_vertex')",
        "2BP01",
        "cannot drop table default_view_dep._ag_label_vertex because other objects depend on it",
    );
    assert_eq!(
        strings(
            engine,
            "SELECT name FROM ag_catalog.ag_label WHERE graph = (SELECT graphid FROM ag_catalog.ag_graph WHERE name = 'default_view_dep') ORDER BY id",
            "name"
        ),
        vec!["_ag_label_vertex", "_ag_label_edge"]
    );
    exec(
        engine,
        "SELECT alter_graph('default_view_dep', 'RENAME', 'default_view_renamed')",
    );
    assert_eq!(
        scalar(engine, "SELECT count(*) FROM default_label_view"),
        Value::Int(0)
    );
    assert_age_error(
        engine,
        "SELECT drop_label('default_view_renamed', '_ag_label_vertex')",
        "2BP01",
        "cannot drop table default_view_renamed._ag_label_vertex because other objects depend on it",
    );
    exec(engine, "DROP VIEW default_label_view");
    exec(
        engine,
        "SELECT drop_label('default_view_renamed', '_ag_label_vertex')",
    );

    exec(engine, "SELECT create_graph('default_view_cascade')");
    exec(
        engine,
        "CREATE VIEW cascade_label_view AS SELECT id FROM default_view_cascade._ag_label_vertex",
    );
    exec(
        engine,
        "CREATE VIEW cascade_label_view_outer AS SELECT id FROM cascade_label_view",
    );
    exec(engine, "SELECT drop_graph('default_view_cascade', true)");
    assert!(engine.view("cascade_label_view").unwrap().is_none());
    assert!(engine.view("cascade_label_view_outer").unwrap().is_none());
}

fn drop_graph_restrict_succeeds_after_all_labels_are_gone(engine: &Engine) {
    exec(engine, "SELECT create_graph('default_drop_restrict')");
    exec(
        engine,
        "SELECT drop_label('default_drop_restrict', '_ag_label_vertex')",
    );
    assert_age_error(
        engine,
        "SELECT drop_graph('default_drop_restrict', false)",
        "2BP01",
        "cannot drop schema default_drop_restrict because other objects depend on it",
    );
    exec(
        engine,
        "SELECT drop_label('default_drop_restrict', '_ag_label_edge')",
    );
    exec(engine, "SELECT drop_graph('default_drop_restrict', false)");
    exec(engine, "SELECT create_graph('default_drop_restrict')");
    assert_eq!(
        strings(
            engine,
            "SELECT name FROM ag_catalog.ag_label WHERE graph = (SELECT graphid FROM ag_catalog.ag_graph WHERE name = 'default_drop_restrict') ORDER BY id",
            "name"
        ),
        vec!["_ag_label_vertex", "_ag_label_edge"]
    );
}

pub(super) fn dropped_vertex_label_and_dangling_edges_persist_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("age-dropped-label.db");
    let endpoints = seed_graph_and_persistent_label_view(&database);
    rename_graph_with_view_across_reopen(&database);
    drop_label_after_view_dependency_reopens(&database);
    verify_broken_graph_and_rename_after_reopen(&database, endpoints);
    verify_renamed_graph_and_recreate(&database);
}

fn seed_graph_and_persistent_label_view(database: &std::path::Path) -> (u64, u64) {
    let engine = Engine::open(database).unwrap();
    exec(&engine, "SELECT create_graph('broken_durable')");
    exec(
        &engine,
        "SELECT * FROM cypher('broken_durable', $$
             CREATE (a:_ag_label_vertex)-[:_ag_label_edge]->(b:_ag_label_vertex)
         $$) AS (ignored agtype)",
    );
    let edge = engine
        .graph_with("broken_durable", |store| {
            store.edges_in_graph("broken_durable").unwrap()
        })
        .unwrap()
        .unwrap()
        .pop()
        .unwrap();
    exec(
        &engine,
        "CREATE VIEW broken_durable_view AS SELECT id FROM broken_durable._ag_label_vertex",
    );
    (edge.source_id, edge.target_id)
}

fn rename_graph_with_view_across_reopen(database: &std::path::Path) {
    {
        let engine = Engine::open(database).unwrap();
        exec(
            &engine,
            "SELECT alter_graph('broken_durable', 'RENAME', 'broken_durable_renamed')",
        );
    }
    let engine = Engine::open(database).unwrap();
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM broken_durable_view"),
        Value::Int(2)
    );
    exec(
        &engine,
        "SELECT alter_graph('broken_durable_renamed', 'RENAME', 'broken_durable')",
    );
}

fn drop_label_after_view_dependency_reopens(database: &std::path::Path) {
    let engine = Engine::open(database).unwrap();
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM broken_durable_view"),
        Value::Int(2)
    );
    assert_age_error(
        &engine,
        "SELECT drop_label('broken_durable', '_ag_label_vertex')",
        "2BP01",
        "cannot drop table broken_durable._ag_label_vertex because other objects depend on it",
    );
    exec(&engine, "DROP VIEW broken_durable_view");
    exec(&engine, "BEGIN");
    exec(
        &engine,
        "SELECT drop_label('broken_durable', '_ag_label_vertex')",
    );
    exec(&engine, "ROLLBACK");
    assert_eq!(
        strings(
            &engine,
            "SELECT name FROM ag_catalog.ag_label WHERE graph = (SELECT graphid FROM ag_catalog.ag_graph WHERE name = 'broken_durable') ORDER BY id",
            "name"
        ),
        vec!["_ag_label_vertex", "_ag_label_edge"]
    );
    exec(
        &engine,
        "SELECT drop_label('broken_durable', '_ag_label_vertex')",
    );
}

fn verify_broken_graph_and_rename_after_reopen(database: &std::path::Path, endpoints: (u64, u64)) {
    let engine = Engine::open(database).unwrap();
    assert_eq!(
        strings(
            &engine,
            "SELECT name FROM ag_catalog.ag_label WHERE graph = (SELECT graphid FROM ag_catalog.ag_graph WHERE name = 'broken_durable') ORDER BY id",
            "name"
        ),
        vec!["_ag_label_edge"]
    );
    let graph_state = engine
        .graph_with("broken_durable", |store| {
            (
                store.vertex_ids_in_graph("broken_durable").unwrap(),
                store.edges_in_graph("broken_durable").unwrap(),
            )
        })
        .unwrap()
        .unwrap();
    assert!(graph_state.0.is_empty());
    assert_eq!(graph_state.1.len(), 1);
    assert_eq!(
        (graph_state.1[0].source_id, graph_state.1[0].target_id),
        endpoints
    );
    assert_age_error(
        &engine,
        "SELECT * FROM cypher('broken_durable', $$ MATCH (n) RETURN count(n) $$) AS (count bigint)",
        "42P01",
        "relation \"broken_durable._ag_label_vertex\" does not exist",
    );
    exec(
        &engine,
        "SELECT alter_graph('broken_durable', 'RENAME', 'broken_renamed')",
    );
}

fn verify_renamed_graph_and_recreate(database: &std::path::Path) {
    let engine = Engine::open(database).unwrap();
    assert!(!engine.has_graph("broken_durable").unwrap());
    assert!(engine.has_graph("broken_renamed").unwrap());
    assert_eq!(
        strings(
            &engine,
            "SELECT name FROM ag_catalog.ag_label WHERE graph = (SELECT graphid FROM ag_catalog.ag_graph WHERE name = 'broken_renamed') ORDER BY id",
            "name"
        ),
        vec!["_ag_label_edge"]
    );
    exec(&engine, "SELECT drop_graph('broken_renamed', true)");
    exec(&engine, "SELECT create_graph('broken_renamed')");
    assert_eq!(
        strings(
            &engine,
            "SELECT name FROM ag_catalog.ag_label WHERE graph = (SELECT graphid FROM ag_catalog.ag_graph WHERE name = 'broken_renamed') ORDER BY id",
            "name"
        ),
        vec!["_ag_label_vertex", "_ag_label_edge"]
    );
}
