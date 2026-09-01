//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apache AGE catalog surface: `LOAD 'age'`, `ag_catalog.ag_graph` /
//! `ag_catalog.ag_label`, the AGE types in `pg_type`, graph and label
//! management functions with AGE's messages and SQLSTATEs, and the
//! namespace, relation, attribute, and sequence rows mirrored into the
//! `PostgreSQL` catalogs.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_graph::LabelKind;
use uqa_sql::SQLResult;

#[path = "sql_age_catalog/default_label_drop.rs"]
mod age_default_label_drop;
#[path = "sql_age_catalog/persistence.rs"]
mod persistence;

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|err| panic!("SQL failed:\n{sql}\n{err:?}"))
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = exec(engine, sql);
    assert_eq!(
        result.rows.len(),
        1,
        "expected one row for:\n{sql}\n{result:?}"
    );
    result.rows[0]
        .values()
        .next()
        .cloned()
        .unwrap_or_else(|| panic!("expected one column for:\n{sql}"))
}

fn strings(engine: &Engine, sql: &str, column: &str) -> Vec<String> {
    exec(engine, sql)
        .rows
        .iter()
        .map(|row| match row.get(column) {
            Some(Value::Str(text)) => text.clone(),
            other => panic!("column {column} is not text in {row:?}: {other:?}"),
        })
        .collect()
}

fn assert_age_error(engine: &Engine, sql: &str, sqlstate: &str, message: &str) {
    let err = match engine.sql(sql, &[]) {
        Ok(result) => panic!("SQL unexpectedly succeeded:\n{sql}\n{result:?}"),
        Err(err) => err,
    };
    assert_eq!(
        err.sqlstate(),
        Some(sqlstate),
        "unexpected SQLSTATE for:\n{sql}\n{err:?}"
    );
    assert_eq!(err.to_string(), message, "unexpected message for:\n{sql}");
}

fn age_session(engine: &Engine) {
    exec(engine, "LOAD 'age'");
    exec(engine, "SET search_path = ag_catalog, \"$user\", public");
}

#[test]
fn load_accepts_embedded_libraries_and_rejects_others_like_postgres() {
    let engine = Engine::new();
    for sql in [
        "LOAD 'age'",
        "LOAD 'age.so'",
        "LOAD '$libdir/age'",
        "LOAD '$libdir/age.so'",
        "LOAD 'plpgsql'",
        "LOAD 'plpgsql.so'",
        "LOAD '$libdir/plpgsql'",
        "LOAD '$libdir/plpgsql.so'",
    ] {
        assert!(exec(&engine, sql).rows.is_empty(), "{sql}");
    }
    assert_age_error(
        &engine,
        "LOAD '/opt/lib/age.so'",
        "58P01",
        "could not access file \"/opt/lib/age.so\": No such file or directory",
    );
}

#[test]
fn ag_catalog_relations_resolve_qualified_and_through_the_search_path() {
    let engine = Engine::new();
    assert!(exec(&engine, "SELECT * FROM ag_catalog.ag_graph")
        .rows
        .is_empty());
    assert!(exec(&engine, "SELECT * FROM ag_catalog.ag_label")
        .rows
        .is_empty());
    let bare = engine.sql("SELECT * FROM ag_graph", &[]).unwrap_err();
    assert!(
        bare.to_string().contains("ag_graph"),
        "bare ag_graph must not resolve without ag_catalog on the search_path: {bare}"
    );

    age_session(&engine);
    exec(&engine, "SELECT create_graph('resolve_demo')");
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM ag_graph WHERE name = 'resolve_demo'"
        ),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM ag_label WHERE graph = (SELECT graphid FROM ag_graph WHERE name = 'resolve_demo')"
        ),
        Value::Int(2)
    );
    let public_qualified = engine
        .sql("SELECT * FROM public.ag_graph", &[])
        .unwrap_err();
    assert!(public_qualified.to_string().contains("ag_graph"));
}

#[test]
fn ag_graph_and_ag_label_report_graphs_and_labels_in_age_shape() {
    let engine = Engine::new();
    age_session(&engine);
    exec(&engine, "SELECT create_graph('catalog_shape')");
    exec(
        &engine,
        "SELECT * FROM cypher('catalog_shape', $$
             CREATE (:Person {name: 'ada'})-[:KNOWS]->(:Person {name: 'bob'}), (:City {name: 'Seoul'})
         $$) AS (ignored agtype)",
    );
    exec(&engine, "SELECT create_vlabel('catalog_shape', 'Company')");
    exec(&engine, "SELECT create_elabel('catalog_shape', 'WORKS_AT')");

    let graphid = assert_ag_graph_row(&engine);
    assert_ag_label_rows(&engine, &graphid);
    assert_person_graphids_carry_label_id_three(&engine);
}

fn assert_ag_graph_row(engine: &Engine) -> Value {
    let graph = exec(
        engine,
        "SELECT graphid, name, namespace FROM ag_catalog.ag_graph",
    );
    assert_eq!(graph.rows.len(), 1);
    let graphid = graph.rows[0].get("graphid").cloned().unwrap();
    assert!(matches!(graphid, Value::Int(oid) if oid > 0));
    assert_eq!(
        graph.rows[0].get("name"),
        Some(&Value::Str("catalog_shape".into()))
    );
    assert_eq!(
        graph.rows[0].get("namespace"),
        Some(&Value::Str("catalog_shape".into()))
    );

    graphid
}

fn assert_ag_label_rows(engine: &Engine, graphid: &Value) {
    let labels = exec(
        engine,
        "SELECT l.name, l.graph, l.id, l.kind, l.relation, l.seq_name
         FROM ag_catalog.ag_label AS l
         JOIN ag_catalog.ag_graph AS g ON g.graphid = l.graph
         WHERE g.name = 'catalog_shape'
         ORDER BY l.id",
    );
    let shaped: Vec<(String, i64, String, String, String)> = labels
        .rows
        .iter()
        .map(|row| {
            assert_eq!(row.get("graph"), Some(graphid));
            (
                text(row.get("name")),
                int(row.get("id")),
                text(row.get("kind")),
                text(row.get("relation")),
                text(row.get("seq_name")),
            )
        })
        .collect();
    assert_eq!(
        shaped,
        vec![
            (
                "_ag_label_vertex".into(),
                1,
                "v".into(),
                "catalog_shape._ag_label_vertex".into(),
                "_ag_label_vertex_id_seq".into()
            ),
            (
                "_ag_label_edge".into(),
                2,
                "e".into(),
                "catalog_shape._ag_label_edge".into(),
                "_ag_label_edge_id_seq".into()
            ),
            (
                "Person".into(),
                3,
                "v".into(),
                "catalog_shape.\"Person\"".into(),
                "Person_id_seq".into()
            ),
            (
                "KNOWS".into(),
                4,
                "e".into(),
                "catalog_shape.\"KNOWS\"".into(),
                "KNOWS_id_seq".into()
            ),
            (
                "City".into(),
                5,
                "v".into(),
                "catalog_shape.\"City\"".into(),
                "City_id_seq".into()
            ),
            (
                "Company".into(),
                6,
                "v".into(),
                "catalog_shape.\"Company\"".into(),
                "Company_id_seq".into()
            ),
            (
                "WORKS_AT".into(),
                7,
                "e".into(),
                "catalog_shape.\"WORKS_AT\"".into(),
                "WORKS_AT_id_seq".into()
            ),
        ]
    );
}

fn assert_person_graphids_carry_label_id_three(engine: &Engine) {
    // The AGE label id is the high 16 bits of every graphid under the label.
    let ids = exec(
        engine,
        "SELECT * FROM cypher('catalog_shape', $$ MATCH (n:Person) RETURN id(n) $$) AS (id bigint)",
    );
    for row in &ids.rows {
        let Some(Value::Int(id)) = row.get("id") else {
            panic!("id must be an integer: {row:?}");
        };
        assert_eq!(id >> 48, 3, "Person vertices carry label id 3: {id}");
    }
}

#[test]
fn age_types_and_domains_are_visible_in_pg_type() {
    let engine = Engine::new();
    let agtype = scalar(&engine, "SELECT oid FROM pg_type WHERE typname = 'agtype'");
    assert_eq!(
        scalar(
            &engine,
            "SELECT typelem FROM pg_type WHERE typname = '_agtype'"
        ),
        agtype
    );
    let namespace = scalar(
        &engine,
        "SELECT oid FROM pg_namespace WHERE nspname = 'ag_catalog'",
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT typnamespace FROM pg_type WHERE typname = 'agtype'"
        ),
        namespace
    );
    let rows = exec(
        &engine,
        "SELECT typname, typtype, typlen, typbyval, typcategory FROM pg_type
         WHERE typnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'ag_catalog')
         ORDER BY typname",
    );
    let shaped: Vec<(String, String, i64, bool, String)> = rows
        .rows
        .iter()
        .map(|row| {
            (
                text(row.get("typname")),
                text(row.get("typtype")),
                int(row.get("typlen")),
                matches!(row.get("typbyval"), Some(Value::Bool(true))),
                text(row.get("typcategory")),
            )
        })
        .collect();
    assert_eq!(
        shaped,
        vec![
            ("_agtype".into(), "b".into(), -1, false, "A".into()),
            ("_graphid".into(), "b".into(), -1, false, "A".into()),
            ("_label_id".into(), "b".into(), -1, false, "A".into()),
            ("_label_kind".into(), "b".into(), -1, false, "A".into()),
            ("agtype".into(), "b".into(), -1, false, "U".into()),
            ("graphid".into(), "b".into(), 8, true, "U".into()),
            ("label_id".into(), "d".into(), 4, true, "N".into()),
            ("label_kind".into(), "d".into(), 1, true, "Z".into()),
        ]
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT typname FROM pg_type WHERE oid = (SELECT typbasetype FROM pg_type WHERE typname = 'label_id')"
        ),
        Value::Str("int4".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT oid FROM pg_type WHERE typname = 'regnamespace'"
        ),
        Value::Int(4089)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT typelem FROM pg_type WHERE typname = '_regnamespace'"
        ),
        Value::Int(4089)
    );
}

#[test]
fn create_graph_and_drop_graph_raise_age_errors() {
    let engine = Engine::new();
    assert_age_error(
        &engine,
        "SELECT create_graph(NULL)",
        "22023",
        "graph name can not be NULL",
    );
    assert_age_error(
        &engine,
        "SELECT create_graph('g1')",
        "22023",
        "graph name is invalid",
    );
    assert_age_error(
        &engine,
        "SELECT create_graph('1abc')",
        "22023",
        "graph name is invalid",
    );
    assert_age_error(
        &engine,
        "SELECT create_graph('abc.')",
        "22023",
        "graph name is invalid",
    );
    assert_age_error(
        &engine,
        "SELECT create_graph('public')",
        "42P06",
        "schema \"public\" already exists",
    );
    exec(&engine, "CREATE SCHEMA taken");
    assert_age_error(
        &engine,
        "SELECT create_graph('taken')",
        "42P06",
        "schema \"taken\" already exists",
    );
    exec(&engine, "SELECT create_graph('my.graph-2')");
    exec(&engine, "SELECT create_graph('lifecycle')");
    assert_age_error(
        &engine,
        "SELECT create_graph('lifecycle')",
        "3F000",
        "graph \"lifecycle\" already exists",
    );
    assert_age_error(
        &engine,
        "SELECT drop_graph('lifecycle')",
        "2BP01",
        "cannot drop schema lifecycle because other objects depend on it",
    );
    assert_age_error(
        &engine,
        "SELECT drop_graph('lifecycle', false)",
        "2BP01",
        "cannot drop schema lifecycle because other objects depend on it",
    );
    assert_age_error(
        &engine,
        "SELECT drop_graph('missing', true)",
        "3F000",
        "graph \"missing\" does not exist",
    );
    assert_age_error(
        &engine,
        "SELECT drop_graph(NULL, true)",
        "22023",
        "graph name can not be NULL",
    );
    assert_eq!(
        scalar(&engine, "SELECT drop_graph('lifecycle', true)"),
        Value::Null
    );
    assert_eq!(
        scalar(&engine, "SELECT ag_catalog.drop_graph('my.graph-2', true)"),
        Value::Null
    );
    assert!(!engine.has_graph("lifecycle").unwrap());
}

#[test]
fn graph_exists_returns_agtype_booleans() {
    let engine = Engine::new();
    exec(&engine, "SELECT create_graph('probe')");
    assert_eq!(
        scalar(&engine, "SELECT graph_exists('probe')"),
        Value::Str("true".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT ag_catalog.graph_exists('missing')"),
        Value::Str("false".into())
    );
    assert_age_error(
        &engine,
        "SELECT graph_exists(NULL)",
        "22023",
        "graph name can not be NULL",
    );
}

#[test]
fn label_functions_follow_age_semantics_and_errors() {
    let engine = Engine::new();
    exec(&engine, "SELECT create_graph('labels')");

    create_label_errors_follow_age(&engine);
    created_labels_keep_ids_and_kinds(&engine);
    drop_label_follows_age(&engine);
    age_default_label_drop::drop_label_removes_entities_and_preserves_incident_edge_rows(&engine);
    age_default_label_drop::default_label_drop_follows_age_restrict_and_broken_graph_lifecycle(
        &engine,
    );
    alter_graph_follows_age(&engine);
}

fn create_label_errors_follow_age(engine: &Engine) {
    // create_vlabel / create_elabel
    for function in ["create_vlabel", "create_elabel"] {
        assert_age_error(
            engine,
            &format!("SELECT {function}(NULL, 'x')"),
            "22023",
            "graph name must not be NULL",
        );
        assert_age_error(
            engine,
            &format!("SELECT {function}('labels', NULL)"),
            "22023",
            "label name must not be NULL",
        );
        assert_age_error(
            engine,
            &format!("SELECT {function}('g1', 'x')"),
            "22023",
            "graph name is invalid",
        );
        assert_age_error(
            engine,
            &format!("SELECT {function}('labels', '1x')"),
            "22023",
            "label name is invalid",
        );
        assert_age_error(
            engine,
            &format!("SELECT {function}('labels', 'has-dash')"),
            "22023",
            "label name is invalid",
        );
        assert_age_error(
            engine,
            &format!("SELECT {function}('missing', 'x')"),
            "3F000",
            "graph \"missing\" does not exist.",
        );
    }
    assert_eq!(
        scalar(engine, "SELECT create_vlabel('labels', 'Person')"),
        Value::Null
    );
    assert_eq!(
        scalar(engine, "SELECT ag_catalog.create_elabel('labels', 'KNOWS')"),
        Value::Null
    );
    assert_age_error(
        engine,
        "SELECT create_vlabel('labels', 'Person')",
        "3F000",
        "label \"Person\" already exists",
    );
    assert_age_error(
        engine,
        "SELECT create_elabel('labels', 'Person')",
        "3F000",
        "label \"Person\" already exists",
    );
    assert_age_error(
        engine,
        "SELECT create_vlabel('labels', '_ag_label_vertex')",
        "3F000",
        "label \"_ag_label_vertex\" already exists",
    );
}

fn created_labels_keep_ids_and_kinds(engine: &Engine) {
    // Pre-registered labels keep their ids when Cypher uses them, and
    // the label kind is enforced the way AGE's CREATE transform does.
    exec(
        engine,
        "SELECT * FROM cypher('labels', $$
             CREATE (:Person {name: 'ada'})-[:KNOWS]->(:Person {name: 'bob'})
         $$) AS (ignored agtype)",
    );
    let person_ids = exec(
        engine,
        "SELECT * FROM cypher('labels', $$ MATCH (n:Person) RETURN id(n) $$) AS (id bigint)",
    );
    assert_eq!(person_ids.rows.len(), 2);
    for row in &person_ids.rows {
        assert_eq!(int(row.get("id")) >> 48, 3);
    }
    let vertex_with_edge_label = engine
        .sql(
            "SELECT * FROM cypher('labels', $$ CREATE (:KNOWS {x: 1}) $$) AS (ignored agtype)",
            &[],
        )
        .unwrap_err();
    assert_eq!(vertex_with_edge_label.sqlstate(), Some("0A000"));
    assert!(
        vertex_with_edge_label
            .to_string()
            .contains("label KNOWS is for edges, not vertices"),
        "{vertex_with_edge_label}"
    );
    let edge_with_vertex_label = engine
        .sql(
            "SELECT * FROM cypher('labels', $$
                 MATCH (a:Person {name: 'ada'}), (b:Person {name: 'bob'})
                 CREATE (a)-[:Person]->(b)
             $$) AS (ignored agtype)",
            &[],
        )
        .unwrap_err();
    assert!(
        edge_with_vertex_label
            .to_string()
            .contains("label Person is for vertices, not edges"),
        "{edge_with_vertex_label}"
    );
    // The reserved names denote the default labels: entities created under
    // them take label ids 1 / 2 and never appear as user labels.
    exec(
        engine,
        "SELECT * FROM cypher('labels', $$
             MATCH (a:Person {name: 'ada'}), (b:Person {name: 'bob'})
             CREATE (:_ag_label_vertex {n: 1}), (a)-[:_ag_label_edge]->(b)
         $$) AS (ignored agtype)",
    );
    let default_ids = exec(
        engine,
        "SELECT * FROM cypher('labels', $$ MATCH (n:_ag_label_vertex) RETURN id(n) $$) AS (id bigint)",
    );
    assert_eq!(default_ids.rows.len(), 1);
    assert_eq!(int(default_ids.rows[0].get("id")) >> 48, 1);
    assert_eq!(
        strings(
            engine,
            "SELECT name FROM ag_catalog.ag_label ORDER BY id",
            "name"
        ),
        vec!["_ag_label_vertex", "_ag_label_edge", "Person", "KNOWS"]
    );
    assert_eq!(
        scalar(engine, "SELECT count(*) FROM labels._ag_label_vertex"),
        Value::Int(3)
    );
    assert_eq!(
        scalar(engine, "SELECT count(*) FROM labels.\"Person\""),
        Value::Int(2)
    );
    assert_eq!(
        scalar(engine, "SELECT count(*) FROM labels._ag_label_edge"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(engine, "SELECT count(*) FROM labels.\"KNOWS\""),
        Value::Int(1)
    );
    let reserved_kind = engine
        .sql(
            "SELECT * FROM cypher('labels', $$ CREATE (:_ag_label_edge {n: 2}) $$) AS (ignored agtype)",
            &[],
        )
        .unwrap_err();
    assert!(
        reserved_kind
            .to_string()
            .contains("label _ag_label_edge is for edges, not vertices"),
        "{reserved_kind}"
    );
}

fn drop_label_follows_age(engine: &Engine) {
    // drop_label
    assert_age_error(
        engine,
        "DROP TABLE labels.\"Person\"",
        "2BP01",
        "table \"Person\" is for label \"Person\"",
    );
    assert_age_error(
        engine,
        "DROP TABLE IF EXISTS labels._ag_label_vertex CASCADE",
        "2BP01",
        "table \"_ag_label_vertex\" is for label \"_ag_label_vertex\"",
    );
    assert_age_error(
        engine,
        "SELECT drop_label(NULL, 'Person')",
        "22023",
        "graph name must not be NULL",
    );
    assert_age_error(
        engine,
        "SELECT drop_label('labels', NULL)",
        "22023",
        "label name must not be NULL",
    );
    assert_age_error(
        engine,
        "SELECT drop_label('missing', 'Person')",
        "3F000",
        "graph \"missing\" does not exist",
    );
    assert_age_error(
        engine,
        "SELECT drop_label('labels', 'Nope')",
        "42P01",
        "label \"Nope\" does not exist",
    );
    assert_age_error(
        engine,
        "SELECT drop_label('labels', 'Person', true)",
        "0A000",
        "force option is not supported yet",
    );
    assert_age_error(
        engine,
        "SELECT drop_label('labels', '_ag_label_edge')",
        "2BP01",
        "cannot drop table labels._ag_label_edge because other objects depend on it",
    );
    exec(
        engine,
        "SELECT * FROM cypher('labels', $$ CREATE (:City {name: 'Seoul'}) $$) AS (ignored agtype)",
    );
    assert_eq!(
        scalar(engine, "SELECT drop_label('labels', 'City')"),
        Value::Null
    );
    // ada, bob, and the unlabeled default-label vertex remain.
    assert_eq!(
        scalar(
            engine,
            "SELECT * FROM cypher('labels', $$ MATCH (n) RETURN count(n) $$) AS (c bigint)"
        ),
        Value::Int(3)
    );
}

fn alter_graph_follows_age(engine: &Engine) {
    // alter_graph
    assert_age_error(
        engine,
        "SELECT alter_graph(NULL, 'RENAME', 'x')",
        "22023",
        "graph_name must not be NULL",
    );
    assert_age_error(
        engine,
        "SELECT alter_graph('labels', NULL, 'x')",
        "22023",
        "operation must not be NULL",
    );
    assert_age_error(
        engine,
        "SELECT alter_graph('labels', 'RENAME', NULL)",
        "22023",
        "new_value must not be NULL",
    );
    assert_age_error(
        engine,
        "SELECT alter_graph('labels', 'DROP', 'x')",
        "22023",
        "invalid operation \"DROP\"",
    );
    assert_age_error(
        engine,
        "SELECT alter_graph('labels', 'rename', 'g1')",
        "22023",
        "new graph name is invalid",
    );
    assert_age_error(
        engine,
        "SELECT alter_graph('missing', 'RENAME', 'renamed')",
        "3F000",
        "graph \"missing\" does not exist",
    );
    exec(engine, "SELECT create_graph('other')");
    assert_age_error(
        engine,
        "SELECT alter_graph('labels', 'RENAME', 'other')",
        "42P06",
        "schema \"other\" already exists",
    );
    assert_age_error(
        engine,
        "SELECT alter_graph('labels', 'RENAME', 'public')",
        "42P06",
        "schema \"public\" already exists",
    );
    assert_age_error(
        engine,
        "SELECT alter_graph('labels', 'RENAME', 'labels')",
        "42P06",
        "schema \"labels\" already exists",
    );
    assert_eq!(
        scalar(engine, "SELECT alter_graph('labels', 'rename', 'renamed')"),
        Value::Null
    );
    assert!(!engine.has_graph("labels").unwrap());
    assert!(engine.has_graph("renamed").unwrap());
    assert_eq!(
        strings(
            engine,
            "SELECT name FROM ag_catalog.ag_graph ORDER BY name",
            "name"
        ),
        vec!["other", "renamed"]
    );
    assert_eq!(
        strings(
            engine,
            "SELECT relation FROM ag_catalog.ag_label WHERE name = 'KNOWS'",
            "relation"
        ),
        vec!["renamed.\"KNOWS\""]
    );
}

#[test]
fn postgres_catalogs_mirror_graph_namespaces_labels_and_sequences() {
    let engine = Engine::new();
    exec(&engine, "SELECT create_graph('mirror')");
    exec(
        &engine,
        "SELECT * FROM cypher('mirror', $$
             CREATE (:Person {name: 'ada'})-[:KNOWS]->(:Person {name: 'bob'}), (:Person {name: 'cid'}), ()
         $$) AS (ignored agtype)",
    );
    exec(&engine, "SELECT create_elabel('mirror', 'WORKS_AT')");

    assert_namespaces_mirror_graphs(&engine);
    assert_pg_class_mirrors_label_relations(&engine);
    assert_pg_attribute_mirrors_label_columns(&engine);
    assert_pg_sequences_mirror_label_sequences(&engine);
    assert_information_schema_mirrors_label_relations(&engine);
}

fn assert_namespaces_mirror_graphs(engine: &Engine) {
    assert_eq!(
        strings(
            engine,
            "SELECT nspname FROM pg_namespace WHERE nspname IN ('ag_catalog', 'mirror') ORDER BY nspname",
            "nspname"
        ),
        vec!["ag_catalog", "mirror"]
    );
    assert_eq!(
        strings(
            engine,
            "SELECT schema_name FROM information_schema.schemata WHERE schema_name IN ('ag_catalog', 'mirror') ORDER BY schema_name",
            "schema_name"
        ),
        vec!["ag_catalog", "mirror"]
    );
}

fn assert_pg_class_mirrors_label_relations(engine: &Engine) {
    let classes = exec(
        engine,
        "SELECT relname, relkind, relnatts, reltuples FROM pg_class
         WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'mirror')
         ORDER BY relname",
    );
    let shaped: Vec<(String, String, i64, i64)> = classes
        .rows
        .iter()
        .map(|row| {
            let tuples = match row.get("reltuples") {
                Some(Value::Float(value)) => *value as i64,
                other => panic!("reltuples must be a float: {other:?}"),
            };
            (
                text(row.get("relname")),
                text(row.get("relkind")),
                int(row.get("relnatts")),
                tuples,
            )
        })
        .collect();
    assert_eq!(
        shaped,
        vec![
            ("KNOWS".into(), "r".into(), 4, 1),
            ("KNOWS_id_seq".into(), "S".into(), 0, 0),
            ("Person".into(), "r".into(), 2, 3),
            ("Person_id_seq".into(), "S".into(), 0, 0),
            ("WORKS_AT".into(), "r".into(), 4, 0),
            ("WORKS_AT_id_seq".into(), "S".into(), 0, 0),
            ("_ag_label_edge".into(), "r".into(), 4, 0),
            ("_ag_label_edge_id_seq".into(), "S".into(), 0, 0),
            ("_ag_label_vertex".into(), "r".into(), 2, 1),
            ("_ag_label_vertex_id_seq".into(), "S".into(), 0, 0),
            ("_label_id_seq".into(), "S".into(), 0, 0),
        ]
    );
}

fn assert_pg_attribute_mirrors_label_columns(engine: &Engine) {
    let attributes = exec(
        engine,
        "SELECT a.attname, t.typname, a.attnotnull FROM pg_attribute AS a
         JOIN pg_class AS c ON c.oid = a.attrelid
         JOIN pg_type AS t ON t.oid = a.atttypid
         WHERE c.relname = 'KNOWS'
         ORDER BY a.attnum",
    );
    let shaped: Vec<(String, String, bool)> = attributes
        .rows
        .iter()
        .map(|row| {
            (
                text(row.get("attname")),
                text(row.get("typname")),
                matches!(row.get("attnotnull"), Some(Value::Bool(true))),
            )
        })
        .collect();
    assert_eq!(
        shaped,
        vec![
            ("id".into(), "graphid".into(), true),
            ("start_id".into(), "graphid".into(), true),
            ("end_id".into(), "graphid".into(), true),
            ("properties".into(), "agtype".into(), false),
        ]
    );
}

fn assert_pg_sequences_mirror_label_sequences(engine: &Engine) {
    let sequences = exec(
        engine,
        "SELECT sequencename, data_type, max_value, last_value FROM pg_sequences
         WHERE schemaname = 'mirror' ORDER BY sequencename",
    );
    let shaped: Vec<(String, String, i64, Option<i64>)> = sequences
        .rows
        .iter()
        .map(|row| {
            (
                text(row.get("sequencename")),
                text(row.get("data_type")),
                int(row.get("max_value")),
                match row.get("last_value") {
                    Some(Value::Int(value)) => Some(*value),
                    Some(Value::Null) => None,
                    other => panic!("last_value: {other:?}"),
                },
            )
        })
        .collect();
    let max_entry = (1_i64 << 48) - 1;
    assert_eq!(
        shaped,
        vec![
            ("KNOWS_id_seq".into(), "bigint".into(), max_entry, Some(1)),
            ("Person_id_seq".into(), "bigint".into(), max_entry, Some(3)),
            ("WORKS_AT_id_seq".into(), "bigint".into(), max_entry, None),
            (
                "_ag_label_edge_id_seq".into(),
                "bigint".into(),
                max_entry,
                None
            ),
            (
                "_ag_label_vertex_id_seq".into(),
                "bigint".into(),
                max_entry,
                Some(1)
            ),
            ("_label_id_seq".into(), "integer".into(), 65_535, Some(5)),
        ]
    );
}

fn assert_information_schema_mirrors_label_relations(engine: &Engine) {
    assert_eq!(
        strings(
            engine,
            "SELECT table_name FROM information_schema.tables WHERE table_schema = 'mirror' ORDER BY table_name",
            "table_name"
        ),
        vec![
            "KNOWS",
            "Person",
            "WORKS_AT",
            "_ag_label_edge",
            "_ag_label_vertex"
        ]
    );
    let columns = exec(
        engine,
        "SELECT column_name, data_type, udt_schema, udt_name FROM information_schema.columns
         WHERE table_schema = 'mirror' AND table_name = 'Person' ORDER BY ordinal_position",
    );
    let shaped: Vec<(String, String, String, String)> = columns
        .rows
        .iter()
        .map(|row| {
            (
                text(row.get("column_name")),
                text(row.get("data_type")),
                text(row.get("udt_schema")),
                text(row.get("udt_name")),
            )
        })
        .collect();
    assert_eq!(
        shaped,
        vec![
            (
                "id".into(),
                "USER-DEFINED".into(),
                "ag_catalog".into(),
                "graphid".into()
            ),
            (
                "properties".into(),
                "USER-DEFINED".into(),
                "ag_catalog".into(),
                "agtype".into()
            ),
        ]
    );
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::Str(text)) => text.clone(),
        other => panic!("expected text, got {other:?}"),
    }
}

fn int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Int(value)) => *value,
        other => panic!("expected integer, got {other:?}"),
    }
}
