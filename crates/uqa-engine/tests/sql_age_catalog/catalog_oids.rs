//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph and label OIDs follow the order Apache AGE 1.8 draws them from the database's counter. `PostgreSQL` 18.4 with AGE 1.8.0 assigns the offsets below, from the first table's OID, to the same statements; the extension's own objects make the absolute values differ.

use std::path::Path;

use uqa_core::Value;
use uqa_engine::Engine;

fn int(engine: &Engine, sql: &str) -> i64 {
    match super::scalar(engine, sql) {
        Value::Int(value) => value,
        other => panic!("{sql}: expected an integer, got {other:?}"),
    }
}

fn class_oid(engine: &Engine, schema: &str, name: &str) -> i64 {
    int(
        engine,
        &format!(
            "SELECT c.oid::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = '{schema}' AND c.relname = '{name}'"
        ),
    )
}

fn row_type(engine: &Engine, schema: &str, name: &str) -> i64 {
    int(
        engine,
        &format!(
            "SELECT c.reltype::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = '{schema}' AND c.relname = '{name}'"
        ),
    )
}

/// Each object with its offset from `marker_before` in `PostgreSQL` 18.4 with AGE 1.8.0.
const EXPECTED_RELATIONS: &[(&str, &str, i64)] = &[
    ("public", "marker_before", 0),
    ("social_graph", "_label_id_seq", 4),
    ("social_graph", "_ag_label_vertex_id_seq", 5),
    ("social_graph", "_ag_label_vertex", 6),
    ("social_graph", "_ag_label_edge_id_seq", 18),
    ("social_graph", "_ag_label_edge", 19),
    ("public", "marker_middle", 35),
    ("social_graph", "Person_id_seq", 38),
    ("social_graph", "Person", 39),
    ("social_graph", "KNOWS_id_seq", 52),
    ("social_graph", "KNOWS", 53),
    ("public", "marker_after", 68),
    ("social_graph", "City_id_seq", 71),
    ("social_graph", "City", 72),
    ("social_graph", "ROAD_id_seq", 85),
    ("social_graph", "ROAD", 86),
    ("public", "marker_end", 101),
];

const EXPECTED_ROW_TYPES: &[(&str, i64)] = &[
    ("_ag_label_vertex", 8),
    ("_ag_label_edge", 21),
    ("Person", 41),
    ("KNOWS", 55),
    ("City", 74),
    ("ROAD", 88),
];

fn build(engine: &Engine) {
    for sql in [
        "CREATE TABLE marker_before (x integer)",
        "SELECT create_graph('social_graph')",
        "CREATE TABLE marker_middle (x integer)",
        "SELECT create_vlabel('social_graph', 'Person')",
        "SELECT create_elabel('social_graph', 'KNOWS')",
        "CREATE TABLE marker_after (x integer)",
        "SELECT * FROM cypher('social_graph', $$ CREATE (:City {name: 'Seoul'})-[:ROAD]->(:City {name: 'Busan'}) $$) AS (v agtype)",
        "CREATE TABLE marker_end (x integer)",
    ] {
        super::exec(engine, sql);
    }
}

fn verify(engine: &Engine) {
    let base = class_oid(engine, "public", "marker_before");
    for (schema, name, offset) in EXPECTED_RELATIONS {
        assert_eq!(
            class_oid(engine, schema, name) - base,
            *offset,
            "{schema}.{name}"
        );
    }
    for (name, offset) in EXPECTED_ROW_TYPES {
        assert_eq!(
            row_type(engine, "social_graph", name) - base,
            *offset,
            "{name}"
        );
    }
    let namespace = int(
        engine,
        "SELECT oid::bigint FROM pg_namespace WHERE nspname = 'social_graph'",
    );
    assert_eq!(namespace - base, 3);
    assert_eq!(
        int(
            engine,
            "SELECT graphid::bigint FROM ag_catalog.ag_graph WHERE name = 'social_graph'"
        ),
        namespace
    );
    assert_eq!(
        int(
            engine,
            "SELECT count(*) FROM ag_catalog.ag_label WHERE graph <> (SELECT graphid FROM ag_catalog.ag_graph WHERE name = 'social_graph')"
        ),
        0
    );
}

#[test]
fn graph_objects_take_oids_in_age_creation_order() {
    let engine = Engine::new();
    build(&engine);
    verify(&engine);
}

#[test]
fn graph_objects_keep_their_oids_across_reopen_and_rename() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("age-catalog-oids.db");
    {
        let engine = Engine::open(&database).unwrap();
        build(&engine);
        verify(&engine);
    }
    let engine = Engine::open(&database).unwrap();
    verify(&engine);
    let person = class_oid(&engine, "social_graph", "Person");
    super::exec(
        &engine,
        "SELECT alter_graph('social_graph', 'RENAME', 'renamed_graph')",
    );
    assert_eq!(class_oid(&engine, "renamed_graph", "Person"), person);
    reopen_keeps(&database, person);
}

fn reopen_keeps(database: &Path, person: i64) {
    let engine = Engine::open(database).unwrap();
    assert_eq!(class_oid(&engine, "renamed_graph", "Person"), person);
}
