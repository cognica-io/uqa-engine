//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A correlated subquery must resolve an outer reference written against the
//! outer table's own name, not only against an alias.
//!
//! A correlated subquery is evaluated against a row that merges the inner
//! relation's columns over the outer row. Inner relations arrive qualified
//! (`allowed.id`) while a plain single-table scan emitted bare names, so
//! `papers.id` had nothing to bind to: the merge overwrote the bare `id` with
//! the inner value and the qualified-lookup fallback then refused the bare name
//! because another qualifier claimed it. The reference evaluated to NULL and
//! the predicate silently matched nothing -- a wrong answer, not an error.

use uqa_engine::Engine;

fn seeded() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE papers (id INTEGER PRIMARY KEY, year INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO papers (id, year) VALUES (1, 2024), (2, 2019), (3, 2025)",
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
fn correlated_exists_resolves_unaliased_outer_table_name() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers \
              WHERE EXISTS (SELECT 1 FROM allowed WHERE allowed.id = papers.id)"
        ),
        vec![1, 2]
    );
}

#[test]
fn correlated_exists_resolves_outer_alias() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT p.id FROM papers AS p \
              WHERE EXISTS (SELECT 1 FROM allowed AS a WHERE a.id = p.id)"
        ),
        vec![1, 2]
    );
}

#[test]
fn correlated_in_subquery_resolves_unaliased_outer_table_name() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers \
              WHERE id IN (SELECT id FROM allowed WHERE allowed.id = papers.id)"
        ),
        vec![1, 2]
    );
}

#[test]
fn correlated_scalar_subquery_resolves_unaliased_outer_table_name() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers \
              WHERE (SELECT COUNT(*) FROM allowed WHERE allowed.id = papers.id) > 0"
        ),
        vec![1, 2]
    );
}

/// The outer reference must bind to the outer row, not to the inner relation's
/// identically named column. If `papers.id` bound to `allowed.id` this would
/// return every row instead of only the seeded one.
#[test]
fn outer_reference_binds_to_the_outer_row_not_the_inner_column() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers WHERE EXISTS (SELECT 1 FROM allowed WHERE papers.id = 1)"
        ),
        vec![1]
    );
}

/// An unqualified column inside the subquery still resolves to the inner
/// relation, so publishing the outer qualifier does not break scope shadowing.
#[test]
fn inner_unqualified_column_still_shadows_the_outer_row() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers WHERE EXISTS (SELECT 1 FROM allowed WHERE id = 2)"
        ),
        vec![1, 2, 3]
    );
}

/// An outer column absent from the inner relation was already resolvable; keep
/// it working.
#[test]
fn outer_only_column_still_resolves() {
    let engine = seeded();
    assert_eq!(
        ids(
            &engine,
            "SELECT id FROM papers WHERE EXISTS (SELECT 1 FROM allowed WHERE papers.year = 2024)"
        ),
        vec![1]
    );
}
