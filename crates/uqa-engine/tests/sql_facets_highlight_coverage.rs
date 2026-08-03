//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Additional facet and highlighting SQL coverage.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_engine::Engine;

fn engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE articles (\
             id SERIAL PRIMARY KEY, \
             title TEXT NOT NULL, \
             body TEXT, \
             category TEXT, \
             author TEXT, \
             year INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX idx_articles_gin ON articles USING gin (title, body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO articles (title, body, category, author, year) VALUES \
             ('Introduction to Database Systems', \
              'A database system provides efficient storage and retrieval of structured data. Modern database engines support SQL queries for data manipulation.', \
              'databases', 'Alice', 2020), \
             ('Information Retrieval Fundamentals', \
              'Information retrieval is the science of searching for information in documents. Full-text search engines use inverted indexes for fast retrieval.', \
              'search', 'Bob', 2021), \
             ('Advanced Query Optimization', \
              'Query optimization transforms SQL queries into efficient execution plans. The database optimizer uses cost-based methods to find the best plan.', \
              'databases', 'Alice', 2022), \
             ('Machine Learning for Search', \
              'Machine learning techniques improve search relevance. Neural retrieval models learn to rank documents using deep learning architectures.', \
              'search', 'Carol', 2023), \
             ('Graph Database Design', \
              'Graph databases store data as vertices and edges. They excel at traversal queries and relationship-heavy workloads.', \
              'databases', 'Bob', 2021), \
             ('Natural Language Processing', \
              'NLP enables computers to understand human language. Text analysis and search applications benefit from NLP techniques.', \
              'nlp', 'Carol', 2022)",
            &[],
        )
        .unwrap();
    engine
}

fn get_str<'a>(row: &'a uqa_sql::ResultRow, column: &str) -> &'a str {
    match row.get(column) {
        Some(Value::Str(value)) => value,
        other => panic!("expected string column {column}, got {other:?}"),
    }
}

fn facet_counts(result: &uqa_sql::SQLResult) -> BTreeMap<String, i64> {
    result
        .rows
        .iter()
        .map(|row| {
            let key = match row.get("facet_value") {
                Some(Value::Str(value)) => value.clone(),
                Some(Value::Int(value)) => value.to_string(),
                other => panic!("unexpected facet value {other:?}"),
            };
            let count = match row.get("facet_count") {
                Some(Value::Int(value)) => *value,
                other => panic!("unexpected facet count {other:?}"),
            };
            (key, count)
        })
        .collect()
}

#[test]
fn sql_highlight_coverage_cases() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, uqa_highlight(body, 'database') AS snippet \
             FROM articles WHERE body @@ 'database'",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        assert!(get_str(row, "snippet").contains("<b>"));
    }

    let result = engine
        .sql(
            "SELECT title, uqa_highlight(body, 'search', '<em>', '</em>') AS snippet \
             FROM articles WHERE body @@ 'search'",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        assert!(get_str(row, "snippet").contains("<em>"));
    }
}

#[test]
fn sql_highlight_no_where_limit_and_null() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT title, uqa_highlight(title, 'graph') AS snippet FROM articles",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 6);
    assert!(result
        .rows
        .iter()
        .any(|row| get_str(row, "snippet").contains("<b>Graph</b>")));

    let result = engine
        .sql(
            "SELECT title, uqa_highlight(body, 'search') AS snippet \
             FROM articles WHERE body @@ 'search' ORDER BY _score DESC LIMIT 2",
            &[],
        )
        .unwrap();
    assert!(result.rows.len() <= 2);

    engine
        .sql(
            "INSERT INTO articles (title, body, category, author, year) \
             VALUES ('Empty Article', NULL, 'misc', 'Dave', 2024)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT title, uqa_highlight(body, 'test') AS snippet \
             FROM articles WHERE title @@ 'empty'",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0].get("snippet"), Some(&Value::Null));
}

#[test]
fn sql_facets_single_and_multi_field() {
    let engine = engine();
    let result = engine
        .sql("SELECT uqa_facets(category) FROM articles", &[])
        .unwrap();
    assert_eq!(result.columns, vec!["facet_value", "facet_count"]);
    let counts = facet_counts(&result);
    assert_eq!(counts["databases"], 3);
    assert_eq!(counts["search"], 2);
    assert_eq!(counts["nlp"], 1);

    let result = engine
        .sql("SELECT uqa_facets(category, author) FROM articles", &[])
        .unwrap();
    assert_eq!(
        result.columns,
        vec!["facet_field", "facet_value", "facet_count"]
    );
    let fields = result
        .rows
        .iter()
        .map(|row| get_str(row, "facet_field"))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(fields.contains("category"));
    assert!(fields.contains("author"));
}

#[test]
fn sql_facets_respect_filters_and_sort_by_value() {
    let engine = engine();
    let result = engine
        .sql(
            "SELECT uqa_facets(category) FROM articles WHERE body @@ 'database'",
            &[],
        )
        .unwrap();
    let total: i64 = facet_counts(&result).values().sum();
    assert!(total > 0);
    assert!(total <= 6);

    let result = engine
        .sql("SELECT uqa_facets(author) FROM articles", &[])
        .unwrap();
    let counts = facet_counts(&result);
    assert_eq!(counts["Alice"], 2);
    assert_eq!(counts["Bob"], 2);
    assert_eq!(counts["Carol"], 2);

    let result = engine
        .sql("SELECT uqa_facets(category) FROM articles", &[])
        .unwrap();
    let values = result
        .rows
        .iter()
        .map(|row| get_str(row, "facet_value"))
        .collect::<Vec<_>>();
    let mut sorted = values.clone();
    sorted.sort_unstable();
    assert_eq!(values, sorted);

    let result = engine
        .sql(
            "SELECT uqa_facets(category) FROM articles WHERE body @@ 'xyznonexistent'",
            &[],
        )
        .unwrap();
    assert!(result.rows.is_empty());
}
