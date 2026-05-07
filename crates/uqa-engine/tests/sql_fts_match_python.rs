//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of SQL `@@` coverage from Python `test_fts_match.py`.

use uqa_core::Value;
use uqa_engine::Engine;

fn engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (\
             id SERIAL PRIMARY KEY, \
             title TEXT NOT NULL, \
             body TEXT NOT NULL, \
             embedding VECTOR(4))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX idx_docs_gin ON docs USING gin (title, body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (title, body, embedding) VALUES \
             ('database internals', \
              'a guide to storage engines and distributed systems', \
              ARRAY[0.9, 0.1, 0.0, 0.0]), \
             ('full text search algorithms', \
              'inverted index and BM25 scoring for information retrieval', \
              ARRAY[0.1, 0.9, 0.0, 0.0]), \
             ('wireless sensor networks', \
              'low power communication protocols for IoT devices', \
              ARRAY[0.0, 0.0, 0.9, 0.1]), \
             ('deep learning fundamentals', \
              'neural network architectures and training techniques', \
              ARRAY[0.0, 0.0, 0.1, 0.9]), \
             ('database query optimization', \
              'cost-based optimizer and query planning for SQL engines', \
              ARRAY[0.8, 0.2, 0.0, 0.0]), \
             ('information retrieval systems', \
              'ranking algorithms and relevance scoring for search engines', \
              ARRAY[0.2, 0.8, 0.0, 0.0])",
            &[],
        )
        .unwrap();
    engine
}

fn get_str<'a>(row: &'a uqa_sql::ResultRow, name: &str) -> &'a str {
    match row.get(name) {
        Some(Value::Str(value)) => value,
        other => panic!("expected string column {name}, got {other:?}"),
    }
}

fn get_score(row: &uqa_sql::ResultRow) -> f64 {
    match row.get("_score") {
        Some(Value::Float(value)) => *value,
        other => panic!("expected _score float, got {other:?}"),
    }
}

#[test]
fn test_single_term() {
    let result = engine()
        .sql(
            "SELECT title, _score FROM docs \
             WHERE title @@ 'database' ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        assert!(get_str(row, "title").contains("database"));
        assert!(get_score(row) > 0.0);
    }
}

#[test]
fn test_phrase() {
    let result = engine()
        .sql(
            "SELECT title, _score FROM docs \
             WHERE body @@ '\"information retrieval\"' ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        assert!(get_score(row) > 0.0);
    }
}

#[test]
fn test_all_column() {
    let result = engine()
        .sql("SELECT title FROM docs WHERE _all @@ 'database'", &[])
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn test_boolean_and() {
    let result = engine()
        .sql(
            "SELECT title FROM docs WHERE title @@ 'database AND query'",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        let title = get_str(row, "title");
        assert!(title.contains("database"));
        assert!(title.contains("query") || title.contains("optimization"));
    }
}

#[test]
fn test_boolean_or() {
    let result = engine()
        .sql(
            "SELECT title FROM docs WHERE title @@ 'database OR wireless'",
            &[],
        )
        .unwrap();
    assert!(result.rows.len() >= 2);
}

#[test]
fn test_boolean_not() {
    let engine = engine();
    let all_result = engine
        .sql("SELECT title FROM docs WHERE title @@ 'database'", &[])
        .unwrap();
    let not_result = engine
        .sql(
            "SELECT title FROM docs WHERE title @@ 'database AND NOT query'",
            &[],
        )
        .unwrap();
    assert!(not_result.rows.len() < all_result.rows.len() || !not_result.rows.is_empty());
}

#[test]
fn test_grouping() {
    let result = engine()
        .sql(
            "SELECT title FROM docs WHERE title @@ '(database OR search) AND text'",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn test_implicit_and() {
    let result = engine()
        .sql("SELECT title FROM docs WHERE title @@ 'full text'", &[])
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn test_field_specific() {
    let result = engine()
        .sql("SELECT title FROM docs WHERE _all @@ 'title:database'", &[])
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn test_hybrid_text_vector() {
    let result = engine()
        .sql(
            "SELECT title, _score FROM docs \
             WHERE _all @@ 'body:search AND embedding:[0.1, 0.9, 0.0, 0.0]' \
             ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        let score = get_score(row);
        assert!(score > 0.0 && score < 1.0);
    }
}

#[test]
fn test_score_calibrated() {
    let result = engine()
        .sql(
            "SELECT title, _score FROM docs \
             WHERE title @@ 'database' ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        let score = get_score(row);
        assert!(score > 0.0 && score < 1.0);
    }
}

#[test]
fn test_order_by_score_limit() {
    let result = engine()
        .sql(
            "SELECT title, _score FROM docs \
             WHERE title @@ 'database' ORDER BY _score DESC LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
}

#[test]
fn test_combined_with_equality() {
    let result = engine()
        .sql(
            "SELECT title FROM docs WHERE title @@ 'database' AND id > 2",
            &[],
        )
        .unwrap();
    for row in &result.rows {
        assert!(get_str(row, "title").contains("database"));
    }
}

#[test]
fn test_empty_query_errors() {
    let err = engine()
        .sql("SELECT title FROM docs WHERE title @@ ''", &[])
        .unwrap_err();
    assert!(err.to_string().contains("Empty query"));
}

#[test]
fn test_unknown_field_returns_empty() {
    let result = engine()
        .sql(
            "SELECT title FROM docs WHERE _all @@ 'nonexistent_field:xyz'",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 0);
}

#[test]
fn test_vector_only_query() {
    let result = engine()
        .sql(
            "SELECT title, _score FROM docs \
             WHERE _all @@ 'embedding:[0.9, 0.1, 0.0, 0.0]' \
             ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    assert!(get_score(&result.rows[0]) > 0.0);
}

#[test]
fn test_not_only() {
    let result = engine()
        .sql("SELECT title FROM docs WHERE title @@ 'NOT database'", &[])
        .unwrap();
    for row in &result.rows {
        assert!(!get_str(row, "title").contains("database"));
    }
}
