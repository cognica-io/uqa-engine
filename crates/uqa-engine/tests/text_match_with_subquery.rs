//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A retrieval predicate must compose with subquery predicates in the same
//! WHERE clause. Combining `text_match` with `IN (SELECT ...)` previously
//! raised `physical scalar subquery slot 0 is out of bounds` because the
//! retrieval rewrite dropped the subquery slots the physical plan had bound.

use uqa_engine::Engine;

fn seeded() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE papers (id INTEGER PRIMARY KEY, title TEXT, abstract TEXT, year INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX papers_abstract_gin ON papers USING gin (abstract)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO papers (id, title, abstract, year) VALUES \
             (1, 'a', 'retrieval ranking sparse index', 2024), \
             (2, 'b', 'retrieval ranking dynamic pruning', 2019), \
             (3, 'c', 'graph pattern matching joins', 2025)",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE TABLE allowed (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO allowed (id) VALUES (1), (2)", &[])
        .unwrap();
    engine
}

fn ids(engine: &Engine, sql: &str) -> Vec<i64> {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("query failed: {sql}\n{error:?}"));
    let mut ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|row| match row.get("id") {
            Some(uqa_core::Value::Int(id)) => Some(*id),
            _ => None,
        })
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn text_match_composes_with_in_subquery() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers \
              WHERE text_match(abstract, 'retrieval ranking') \
                AND id IN (SELECT id FROM allowed)"
        ),
        vec![1, 2]
    );
}

#[test]
fn text_match_composes_with_not_in_subquery() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers \
              WHERE text_match(abstract, 'retrieval ranking') \
                AND id NOT IN (SELECT id FROM allowed)"
        ),
        Vec::<i64>::new()
    );
}

#[test]
fn text_match_composes_with_exists_subquery() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers \
              WHERE text_match(abstract, 'retrieval ranking') \
                AND EXISTS (SELECT 1 FROM allowed WHERE allowed.id = 1)"
        ),
        vec![1, 2]
    );
}

#[test]
fn text_match_composes_with_scalar_subquery_comparison() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers \
              WHERE text_match(abstract, 'retrieval ranking') \
                AND year > (SELECT MIN(id) + 2019 FROM allowed)"
        ),
        vec![1]
    );
}

#[test]
fn in_subquery_without_text_match_still_works() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers WHERE id IN (SELECT id FROM allowed)"
        ),
        vec![1, 2]
    );
}

/// A retrieval predicate combined with a correlated subquery must still reach
/// the retrieval machinery. Qualifier pushdown previously bailed out for the
/// whole WHERE clause as soon as it mentioned a subquery anywhere, leaving
/// `text_match` in the residual scalar filter where it cannot be evaluated.
#[test]
fn text_match_composes_with_correlated_exists_on_aliased_source() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT p.id FROM papers AS p \
              WHERE text_match(p.abstract, 'retrieval ranking') \
                AND EXISTS (SELECT 1 FROM allowed AS a WHERE a.id = p.id)"
        ),
        vec![1, 2]
    );
}

#[test]
fn text_match_composes_with_correlated_exists_on_unaliased_source() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers \
              WHERE text_match(abstract, 'retrieval ranking') \
                AND EXISTS (SELECT 1 FROM allowed WHERE allowed.id = papers.id)"
        ),
        vec![1, 2]
    );
}

#[test]
fn bayesian_match_composes_with_correlated_in_subquery() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT p.id FROM papers AS p \
              WHERE bayesian_match(p.abstract, 'retrieval ranking') \
                AND p.id IN (SELECT a.id FROM allowed AS a WHERE a.id = p.id)"
        ),
        vec![1, 2]
    );
}
