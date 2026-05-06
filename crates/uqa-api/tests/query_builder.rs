//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for the `QueryBuilder` fluent API.

use uqa_api::{Order, QueryBuilder};
use uqa_core::Value;
use uqa_engine::Engine;

fn engine_with_corpus() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, qty INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO notes (id, title, qty) VALUES \
             (1, 'rust async', 7), \
             (2, 'python web', 3), \
             (3, 'rust embedded', 12), \
             (4, 'go networking', 5)",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn select_with_text_match_and_order_runs() {
    let engine = engine_with_corpus();
    let result = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "title"])
        .text_match("title", "rust")
        .order_by_desc("_score")
        .limit(5)
        .execute()
        .unwrap();
    let titles: Vec<String> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("title") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(titles.iter().any(|t| t.contains("rust")));
}

#[test]
fn where_filters_compose_with_and() {
    let engine = engine_with_corpus();
    let result = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .where_gt("qty", &Value::Int(4))
        .where_lt("qty", &Value::Int(10))
        .order_by_asc("id")
        .execute()
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    // qty range (4, 10) -> rows with qty=7 and qty=5 -> ids 1 and 4.
    assert_eq!(ids, vec![1, 4]);
}

#[test]
fn to_sql_renders_full_clause() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "title"])
        .where_eq("id", &Value::Int(2))
        .order_by("id", Order::Asc)
        .limit(3)
        .offset(1)
        .to_sql();
    assert_eq!(
        sql,
        "SELECT id, title FROM notes WHERE id = 2 ORDER BY id ASC LIMIT 3 OFFSET 1"
    );
}

#[test]
fn multi_field_match_through_builder() {
    let engine = engine_with_corpus();
    let result = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .multi_field_match(&[("title", "rust"), ("title", "embedded")])
        .order_by_desc("_score")
        .execute()
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn fuse_attention_renders_attention_call() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .fuse_attention(&["text_match('rust')", "text_match('embedded')"])
        .to_sql();
    assert!(sql.contains("attention(text_match('rust'), text_match('embedded'))"));
}

#[test]
fn fuse_learned_quotes_model_name_and_includes_signals() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .fuse_learned("my_model", &["text_match('a')", "knn_match('v', '[1]', 5)"])
        .to_sql();
    assert!(sql.contains("learned_fusion('my_model', text_match('a'), knn_match('v', '[1]', 5))"));
}

#[test]
fn rpq_replaces_from_clause() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "_unused")
        .select_columns(&["*"])
        .rpq("manages*", 1, "g")
        .to_sql();
    assert!(sql.contains("FROM rpq('manages*', 1, 'g')"));
}

#[test]
fn highlight_and_facets_render_function_calls() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .highlight("title", "rust")
        .facets(&["author", "year"])
        .to_sql();
    assert!(sql.contains("uqa_highlight('title', 'rust')"));
    assert!(sql.contains("uqa_facets('author', 'year')"));
}

#[test]
fn bayesian_match_renders_where_clause() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .bayesian_match("title", "rust")
        .to_sql();
    assert!(sql.contains("WHERE bayesian_match(title, 'rust')"));
}

#[test]
fn explain_returns_plan_lines() {
    let engine = engine_with_corpus();
    let plan = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .where_eq("id", &Value::Int(1))
        .limit(2)
        .explain()
        .unwrap();
    assert!(plan.contains("Select"));
    assert!(plan.contains("limit=2"));
}
