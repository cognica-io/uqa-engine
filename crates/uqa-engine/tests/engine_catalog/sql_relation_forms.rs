//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 relation persistence, temporary lifecycle, materialized views, and view reloptions.

use uqa_core::{ArrayValue, Value};
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn scalar(engine: &Engine, sql: &str, column: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.rows[0][column].clone()
}

fn text_array(values: &[&str]) -> Value {
    Value::Array(
        ArrayValue::try_new(
            values
                .iter()
                .map(|value| Value::Str((*value).into()))
                .collect(),
        )
        .unwrap(),
    )
}

#[test]
fn temporary_tables_apply_on_commit_actions_and_rollback_creation() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("on-commit.db")).unwrap();
    exec(
        &engine,
        "CREATE TEMP TABLE preserved (id INTEGER) ON COMMIT PRESERVE ROWS",
    );
    exec(&engine, "INSERT INTO preserved VALUES (1)");
    assert_eq!(
        scalar(&engine, "SELECT count(*) AS n FROM preserved", "n"),
        Value::Int(1)
    );

    exec(&engine, "BEGIN");
    exec(
        &engine,
        "CREATE TEMP TABLE cleared (id INTEGER) ON COMMIT DELETE ROWS",
    );
    exec(&engine, "INSERT INTO cleared VALUES (1), (2)");
    assert_eq!(
        scalar(&engine, "SELECT count(*) AS n FROM cleared", "n"),
        Value::Int(2)
    );
    exec(&engine, "COMMIT");
    assert_eq!(
        scalar(&engine, "SELECT count(*) AS n FROM cleared", "n"),
        Value::Int(0)
    );

    exec(&engine, "BEGIN");
    exec(
        &engine,
        "CREATE TEMP TABLE dropped (id INTEGER) ON COMMIT DROP",
    );
    exec(&engine, "INSERT INTO dropped VALUES (1)");
    exec(&engine, "COMMIT");
    assert_eq!(
        engine
            .sql("SELECT * FROM dropped", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );

    exec(
        &engine,
        "CREATE TEMP TABLE auto_cleared (id INTEGER) ON COMMIT DELETE ROWS",
    );
    exec(&engine, "INSERT INTO auto_cleared VALUES (1)");
    assert_eq!(
        scalar(&engine, "SELECT count(*) AS n FROM auto_cleared", "n"),
        Value::Int(0)
    );
    exec(
        &engine,
        "CREATE TEMP TABLE auto_dropped (id INTEGER) ON COMMIT DROP",
    );
    assert_eq!(
        engine
            .sql("SELECT * FROM auto_dropped", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );

    exec(&engine, "BEGIN");
    exec(
        &engine,
        "CREATE TEMP TABLE cascade_parent (id INTEGER PRIMARY KEY) ON COMMIT DROP",
    );
    exec(
        &engine,
        "CREATE TEMP TABLE cascade_child (parent_id INTEGER REFERENCES cascade_parent(id))",
    );
    exec(
        &engine,
        "CREATE VIEW cascade_view AS SELECT id FROM cascade_parent",
    );
    exec(
        &engine,
        "CREATE VIEW nested_cascade_view AS SELECT id FROM cascade_view",
    );
    exec(&engine, "COMMIT");
    for relation in ["cascade_parent", "cascade_view", "nested_cascade_view"] {
        assert_eq!(
            engine
                .sql(&format!("SELECT * FROM {relation}"), &[])
                .unwrap_err()
                .sqlstate(),
            Some("42P01")
        );
    }
    assert!(engine.has_table("cascade_child").unwrap());
    assert!(engine.foreign_keys("cascade_child").unwrap().is_empty());

    exec(&engine, "BEGIN");
    exec(&engine, "CREATE TEMP TABLE rolled_back (id INTEGER)");
    exec(&engine, "ROLLBACK");
    assert_eq!(
        engine
            .sql("SELECT * FROM rolled_back", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );
}

fn assert_temporary_catalog_identity(engine: &Engine) {
    let persistence = engine
        .sql(
            "SELECT relname, relpersistence FROM pg_catalog.pg_class WHERE relname IN ('source', 'copied', 'visible', 'implicitly_temporary', 'local_ids') ORDER BY relname",
            &[],
        )
        .unwrap();
    assert_eq!(persistence.rows.len(), 5);
    assert!(persistence
        .rows
        .iter()
        .all(|row| row["relpersistence"] == Value::Str("t".into())));
    for sql in [
        "SELECT count(*) AS n FROM pg_catalog.pg_namespace WHERE nspname LIKE 'pg_temp_%'",
        "SELECT count(*) AS n FROM pg_catalog.pg_attribute AS a JOIN pg_catalog.pg_class AS c ON c.oid = a.attrelid WHERE c.relname = 'source' AND a.attname = 'id'",
        "SELECT count(*) AS n FROM pg_catalog.pg_sequences WHERE sequencename = 'local_ids'",
    ] {
        assert_eq!(scalar(engine, sql, "n"), Value::Int(1), "{sql}");
    }
}

fn assert_temporary_relations_are_session_local(database: &std::path::Path) {
    let observer = Engine::open(database).unwrap();
    for relation in ["source", "copied", "visible", "implicitly_temporary"] {
        assert_eq!(
            observer
                .sql(&format!("SELECT * FROM {relation}"), &[])
                .unwrap_err()
                .sqlstate(),
            Some("42P01")
        );
    }
    assert_eq!(
        observer
            .sql("SELECT nextval('local_ids')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );
    assert_eq!(
        scalar(
            &observer,
            "SELECT count(*) AS n FROM pg_catalog.pg_class WHERE relname IN ('source', 'copied', 'visible', 'implicitly_temporary', 'local_ids')",
            "n",
        ),
        Value::Int(0)
    );
}

#[test]
fn temporary_table_view_sequence_and_ctas_are_session_local_and_discardable() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("temporary-relations.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(&engine, "CREATE TEMP TABLE source (id INTEGER)");
        exec(&engine, "INSERT INTO source VALUES (7)");
        exec(&engine, "CREATE TEMP TABLE copied AS SELECT id FROM source");
        exec(&engine, "CREATE TEMP VIEW visible AS SELECT id FROM copied");
        exec(
            &engine,
            "CREATE VIEW implicitly_temporary AS SELECT id FROM source",
        );
        assert_eq!(
            engine
                .sql(
                    "CREATE MATERIALIZED VIEW invalid_snapshot AS SELECT id FROM source",
                    &[],
                )
                .unwrap_err()
                .sqlstate(),
            Some("0A000")
        );
        exec(&engine, "CREATE TEMP SEQUENCE local_ids START 9");
        assert_eq!(
            scalar(&engine, "SELECT id FROM visible", "id"),
            Value::Int(7)
        );
        assert_eq!(
            scalar(&engine, "SELECT nextval('local_ids') AS id", "id"),
            Value::Int(9)
        );
        assert_temporary_catalog_identity(&engine);
        assert_temporary_relations_are_session_local(&database);
        exec(&engine, "DISCARD TEMP");
        for relation in ["source", "copied", "visible", "implicitly_temporary"] {
            assert!(engine
                .sql(&format!("SELECT * FROM {relation}"), &[])
                .is_err());
        }
        assert!(engine.sql("SELECT nextval('local_ids')", &[]).is_err());
        exec(&engine, "BEGIN");
        exec(&engine, "CREATE TEMP TABLE transactional_temp (id INTEGER)");
        exec(&engine, "SAVEPOINT before_insert");
        exec(&engine, "INSERT INTO transactional_temp VALUES (1)");
        exec(&engine, "ROLLBACK TO SAVEPOINT before_insert");
        assert_eq!(
            scalar(&engine, "SELECT count(*) AS n FROM transactional_temp", "n"),
            Value::Int(0)
        );
        exec(&engine, "ROLLBACK");
        assert!(engine.sql("SELECT * FROM transactional_temp", &[]).is_err());
    }
    let reopened = Engine::open(&database).unwrap();
    for relation in ["source", "copied", "visible", "implicitly_temporary"] {
        assert!(reopened
            .sql(&format!("SELECT * FROM {relation}"), &[])
            .is_err());
    }
    assert!(reopened.sql("SELECT nextval('local_ids')", &[]).is_err());
}

#[test]
fn unlogged_table_and_sequence_keep_catalog_identity_across_clean_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("unlogged-relations.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE UNLOGGED TABLE events (id INTEGER, embedding VECTOR(2))",
        );
        exec(&engine, "INSERT INTO events VALUES (4, ARRAY[1.0, 0.0])");
        exec(&engine, "CREATE UNLOGGED SEQUENCE event_ids START 12");
        assert_eq!(engine.nextval("event_ids").unwrap(), 12);
    }
    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT id FROM events", "id"),
        Value::Int(4)
    );
    assert_eq!(reopened.nextval("event_ids").unwrap(), 13);
    let hits = reopened
        .knn_search("events", "embedding", vec![1.0, 0.0], 1)
        .unwrap();
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(4));
    let classes = reopened
        .sql(
            "SELECT relname, relpersistence FROM pg_catalog.pg_class WHERE relname IN ('events', 'event_ids') ORDER BY relname",
            &[],
        )
        .unwrap();
    assert_eq!(classes.rows.len(), 2);
    assert!(classes
        .rows
        .iter()
        .all(|row| row["relpersistence"] == Value::Str("u".into())));
}

#[test]
fn materialized_view_is_stale_until_refresh_and_tracks_population() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE base (id INTEGER)");
    exec(&engine, "INSERT INTO base VALUES (1), (2)");
    exec(
        &engine,
        "CREATE MATERIALIZED VIEW snapshot AS SELECT id FROM base",
    );
    assert_eq!(
        scalar(&engine, "SELECT count(*) AS n FROM snapshot", "n"),
        Value::Int(2)
    );
    exec(&engine, "INSERT INTO base VALUES (3)");
    assert_eq!(
        scalar(&engine, "SELECT count(*) AS n FROM snapshot", "n"),
        Value::Int(2)
    );
    exec(&engine, "REFRESH MATERIALIZED VIEW snapshot");
    assert_eq!(
        scalar(&engine, "SELECT count(*) AS n FROM snapshot", "n"),
        Value::Int(3)
    );
    exec(&engine, "REFRESH MATERIALIZED VIEW snapshot WITH NO DATA");
    let unpopulated = engine.sql("SELECT * FROM snapshot", &[]).unwrap_err();
    assert_eq!(unpopulated.sqlstate(), Some("55000"));
    assert_eq!(
        scalar(
            &engine,
            "SELECT relispopulated FROM pg_catalog.pg_class WHERE relname = 'snapshot'",
            "relispopulated",
        ),
        Value::Bool(false)
    );
    exec(&engine, "REFRESH MATERIALIZED VIEW snapshot WITH DATA");
    let class = engine
        .sql(
            "SELECT relkind, relpersistence, relispopulated FROM pg_catalog.pg_class WHERE relname = 'snapshot'",
            &[],
        )
        .unwrap();
    assert_eq!(class.rows[0]["relkind"], Value::Str("m".into()));
    assert_eq!(class.rows[0]["relpersistence"], Value::Str("p".into()));
    assert_eq!(class.rows[0]["relispopulated"], Value::Bool(true));
    let matview = engine
        .sql(
            "SELECT schemaname, matviewname, hasindexes, ispopulated FROM pg_catalog.pg_matviews WHERE matviewname = 'snapshot'",
            &[],
        )
        .unwrap();
    assert_eq!(matview.rows.len(), 1);
    assert_eq!(matview.rows[0]["schemaname"], Value::Str("public".into()));
    assert_eq!(matview.rows[0]["hasindexes"], Value::Bool(false));
    assert_eq!(matview.rows[0]["ispopulated"], Value::Bool(true));
}

#[test]
fn materialized_view_and_view_options_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("view-forms.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(&engine, "CREATE TABLE base (id INTEGER)");
        exec(&engine, "INSERT INTO base VALUES (5)");
        exec(
            &engine,
            "CREATE MATERIALIZED VIEW snapshot WITH (fillfactor=80) AS SELECT id FROM base",
        );
        exec(
            &engine,
            "ALTER MATERIALIZED VIEW snapshot SET (fillfactor=75)",
        );
        exec(
            &engine,
            "CREATE VIEW configured WITH (security_barrier=true, security_invoker=on) AS SELECT id FROM base",
        );
        exec(
            &engine,
            "ALTER VIEW configured SET (security_barrier=false)",
        );
        exec(&engine, "ALTER VIEW configured RESET (security_invoker)");
        exec(
            &engine,
            "CREATE VIEW replaceable WITH (security_barrier=true) AS SELECT id FROM base",
        );
        exec(
            &engine,
            "CREATE OR REPLACE VIEW replaceable AS SELECT id FROM base",
        );
    }
    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT id FROM snapshot", "id"),
        Value::Int(5)
    );
    let options = reopened
        .sql(
            "SELECT relname, reloptions FROM pg_catalog.pg_class WHERE relname IN ('configured', 'replaceable', 'snapshot') ORDER BY relname",
            &[],
        )
        .unwrap();
    assert_eq!(options.rows[0]["relname"], Value::Str("configured".into()));
    assert_eq!(
        options.rows[0]["reloptions"],
        text_array(&["security_barrier=false"])
    );
    assert_eq!(options.rows[1]["relname"], Value::Str("replaceable".into()));
    assert_eq!(options.rows[1]["reloptions"], Value::Null);
    assert_eq!(options.rows[2]["relname"], Value::Str("snapshot".into()));
    assert_eq!(
        options.rows[2]["reloptions"],
        text_array(&["fillfactor=75"])
    );
}

#[test]
fn relation_form_wrong_kind_and_option_errors_use_postgresql_states() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE ordinary (id INTEGER)");
    exec(&engine, "CREATE VIEW plain AS SELECT id FROM ordinary");
    exec(
        &engine,
        "CREATE MATERIALIZED VIEW materialized AS SELECT id FROM ordinary",
    );
    exec(&engine, "CREATE SEQUENCE occupied_sequence");
    for (sql, state) in [
        ("REFRESH MATERIALIZED VIEW ordinary", "0A000"),
        ("DROP MATERIALIZED VIEW ordinary", "42809"),
        ("DROP VIEW materialized", "42809"),
        ("ALTER VIEW ordinary SET (security_barrier=true)", "42809"),
        ("ALTER VIEW plain SET (unknown_option=true)", "22023"),
        ("ALTER VIEW plain SET (security_barrier=maybe)", "22023"),
        (
            "CREATE TABLE plain AS SELECT * FROM relation_that_does_not_exist",
            "42P07",
        ),
        (
            "CREATE TABLE occupied_sequence AS SELECT * FROM relation_that_does_not_exist",
            "42P07",
        ),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    }
    exec(
        &engine,
        "CREATE TABLE IF NOT EXISTS plain AS SELECT * FROM relation_that_does_not_exist",
    );
    exec(&engine, "DROP MATERIALIZED VIEW materialized");
    exec(&engine, "DROP VIEW plain");
}
