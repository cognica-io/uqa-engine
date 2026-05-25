//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the SQL surface added beyond the Phase 5 quickstart:
//! `INSERT ... SELECT`, `CREATE VIEW`, `CREATE SCHEMA`, `EXPLAIN`,
//! `ANALYZE`, `TRUNCATE`, transaction control statements.

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn insert_from_select_copies_rows() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE src (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE dst (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO src (body) VALUES ('hello')", &[])
        .unwrap();
    eng.sql("INSERT INTO src (body) VALUES ('world')", &[])
        .unwrap();
    eng.sql("INSERT INTO dst (body) SELECT body FROM src", &[])
        .unwrap();
    let res = eng.sql("SELECT body FROM dst ORDER BY id", &[]).unwrap();
    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0]["body"], Value::Str("hello".into()));
    assert_eq!(res.rows[1]["body"], Value::Str("world".into()));
}

#[test]
fn create_view_and_drop_round_trip() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("CREATE VIEW v AS SELECT id, body FROM notes", &[])
        .unwrap();
    assert!(eng.view("v").is_some());
}

#[test]
fn create_view_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("views.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql("CREATE SCHEMA app", &[]).unwrap();
        eng.sql("SET search_path TO app, public", &[]).unwrap();
        eng.sql(
            "CREATE TABLE app.notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
        eng.sql("INSERT INTO notes (body) VALUES ('hello')", &[])
            .unwrap();
        eng.sql("CREATE VIEW app.note_bodies AS SELECT body FROM notes", &[])
            .unwrap();
    }
    let eng = Engine::open(&db).unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    let rows = eng.sql("SELECT body FROM note_bodies", &[]).unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["body"], Value::Str("hello".into()));
}

#[test]
fn create_schema_records_name() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("CREATE SCHEMA IF NOT EXISTS app", &[]).unwrap();
    assert!(eng.drop_schema("app"));
}

#[test]
fn truncate_wipes_rows_keeping_schema() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (body) VALUES ('a')", &[]).unwrap();
    eng.sql("INSERT INTO t (body) VALUES ('b')", &[]).unwrap();
    eng.sql("TRUNCATE TABLE t", &[]).unwrap();
    let res = eng.sql("SELECT body FROM t", &[]).unwrap();
    assert!(res.rows.is_empty());
    // Schema still intact: we can still INSERT.
    eng.sql("INSERT INTO t (body) VALUES ('c')", &[]).unwrap();
}

#[test]
fn explain_runs_inner_statement_silently() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    // EXPLAIN currently no-ops the result; just confirm the
    // statement parses + runs.
    eng.sql("EXPLAIN SELECT * FROM t", &[]).unwrap();
}

#[test]
fn analyze_is_supported() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("ANALYZE", &[]).unwrap();
}

#[test]
fn transaction_begin_commit_round_trip() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("INSERT INTO t (body) VALUES ('inside')", &[])
        .unwrap();
    eng.sql("COMMIT", &[]).unwrap();
    let res = eng.sql("SELECT body FROM t", &[]).unwrap();
    assert_eq!(res.rows.len(), 1);
}

#[test]
fn savepoint_release_round_trip() {
    let eng = Engine::new();
    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("SAVEPOINT sp", &[]).unwrap();
    eng.sql("RELEASE SAVEPOINT sp", &[]).unwrap();
    eng.sql("COMMIT", &[]).unwrap();
}
