//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `create_analyzer` / `drop_analyzer` / `list_analyzers` /
//! `set_table_analyzer` table-functions.

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn create_then_list_analyzer() {
    let eng = Engine::new();
    eng.sql(
        "SELECT * FROM create_analyzer('strict', '{\"tokenizer\":\"keyword\"}')",
        &[],
    )
    .unwrap();
    let r = eng.sql("SELECT * FROM list_analyzers()", &[]).unwrap();
    let names: Vec<String> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("analyzer_name") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"strict".to_string()));
}

#[test]
fn drop_analyzer_removes_entry() {
    let eng = Engine::new();
    eng.sql(
        "SELECT * FROM create_analyzer('strict', '{\"tokenizer\":\"keyword\"}')",
        &[],
    )
    .unwrap();
    eng.sql("SELECT * FROM drop_analyzer('strict')", &[])
        .unwrap();
    let r = eng.sql("SELECT * FROM list_analyzers()", &[]).unwrap();
    let names: Vec<String> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("analyzer_name") {
            Some(uqa_core::Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(!names.contains(&"strict".to_string()));
}

#[test]
fn set_table_analyzer_records_assignment() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "SELECT * FROM create_analyzer('strict', '{\"tokenizer\":\"keyword\"}')",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT * FROM set_table_analyzer('docs', 'title', 'strict')",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    let recorded = eng.table_field_analyzer("docs", "title").unwrap();
    assert_eq!(recorded.0, "strict");
    assert_eq!(recorded.1, "both");
}
