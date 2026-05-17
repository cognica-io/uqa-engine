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
