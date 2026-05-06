//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema.tables` / `information_schema.columns` and
//! `pg_catalog.pg_tables` virtual views.

use uqa_engine::Engine;

#[test]
fn information_schema_tables_lists_user_tables() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE owners (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT table_name FROM information_schema.tables ORDER BY table_name",
            &[],
        )
        .unwrap();
    let names: Vec<String> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("table_name") {
            Some(uqa_core::Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"accounts".to_string()));
    assert!(names.contains(&"owners".to_string()));
}

#[test]
fn information_schema_columns_lists_each_column() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER, owner TEXT)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT column_name, ordinal_position FROM information_schema.columns \
             WHERE table_name = 'accounts'",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 3);
}

#[test]
fn pg_tables_lists_user_tables() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE accounts (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let r = eng
        .sql("SELECT tablename FROM pg_catalog.pg_tables", &[])
        .unwrap();
    let names: Vec<String> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("tablename") {
            Some(uqa_core::Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"accounts".to_string()));
}
