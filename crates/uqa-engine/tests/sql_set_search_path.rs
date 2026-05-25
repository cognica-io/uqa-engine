//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SET search_path TO ...` parsing/execution and the namespace
//! introspection accessors. Mirrors the canonical UQA implementation's
//! `Engine._tables.search_path / list_schemas / tables_in_schema`.

use uqa_engine::Engine;

#[test]
fn search_path_default_is_public_only() {
    let eng = Engine::new();
    assert_eq!(eng.search_path(), vec!["public".to_string()]);
}

#[test]
fn set_search_path_via_sql_updates_resolution_order() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    assert_eq!(
        eng.search_path(),
        vec!["app".to_string(), "public".to_string()]
    );
}

#[test]
fn tables_in_schema_buckets_qualified_names() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("CREATE TABLE app.users (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("CREATE TABLE plain (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let app_tables = eng.tables_in_schema("app");
    assert_eq!(app_tables, vec!["users".to_string()]);
    let pub_tables = eng.tables_in_schema("public");
    assert!(pub_tables.contains(&"plain".to_string()));
}

#[test]
fn search_path_resolves_qualified_tables_by_unqualified_name() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();

    eng.sql("INSERT INTO users (id, name) VALUES (1, 'Alice')", &[])
        .unwrap();
    eng.sql(
        "ALTER TABLE users ADD COLUMN active BOOLEAN DEFAULT TRUE",
        &[],
    )
    .unwrap();
    let rows = eng
        .sql("SELECT name, active FROM users WHERE id = 1", &[])
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], uqa_core::Value::Str("Alice".into()));
    assert_eq!(rows[0]["active"], uqa_core::Value::Bool(true));
}

#[test]
fn search_path_resolves_views_sequences_and_foreign_tables() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO users (id, name) VALUES (1, 'Alice')", &[])
        .unwrap();
    eng.sql("CREATE VIEW app.user_names AS SELECT name FROM users", &[])
        .unwrap();
    let view_rows = eng.sql("SELECT name FROM user_names", &[]).unwrap().rows;
    assert_eq!(view_rows[0]["name"], uqa_core::Value::Str("Alice".into()));

    eng.sql("CREATE SEQUENCE app.user_seq START 10", &[])
        .unwrap();
    let seq = eng.sql("SELECT nextval('user_seq') AS v", &[]).unwrap();
    assert_eq!(seq.rows[0]["v"], uqa_core::Value::Int(10));

    eng.sql(
        "CREATE SERVER mem FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FOREIGN TABLE app.remote_users (id INTEGER, name TEXT) \
         SERVER mem OPTIONS (source 'memory')",
        &[],
    )
    .unwrap();
    assert!(eng.foreign_table("remote_users").is_some());
}
