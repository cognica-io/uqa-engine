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

#[test]
fn correlated_subquery_keeps_quoted_dotted_qualifier_and_column_structured() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE \"outer.table\" (\"column.dot\" INTEGER)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO \"outer.table\" (\"column.dot\") VALUES (7)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT (SELECT \"outer.alias\".\"column.dot\" + 1) AS value
             FROM \"outer.table\" AS \"outer.alias\"",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["value"], uqa_core::Value::Int(8));
}

#[test]
fn correlated_subquery_rejects_an_ambiguous_current_scope_column() {
    let engine = seeded();
    let error = engine
        .sql(
            "SELECT id FROM papers
             WHERE (
                 SELECT id
                 FROM allowed a JOIN allowed b ON a.id = b.id
                 WHERE papers.id > 0
                 LIMIT 1
             ) = papers.id",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42702"));
}

#[test]
fn correlated_subquery_preserves_outer_scope_ambiguity() {
    let engine = seeded();
    let error = engine
        .sql(
            "SELECT 1
             FROM papers p JOIN allowed a ON true
             WHERE EXISTS (SELECT 1 WHERE id = 1)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42702"));
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

#[test]
fn correlated_subquery_in_window_partition_uses_the_physical_outer_row() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE window_outer (n INTEGER)", &[])
        .unwrap();
    engine
        .sql("CREATE TABLE window_inner (x INTEGER)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO window_outer VALUES (1), (3)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO window_inner VALUES (1), (2), (3)", &[])
        .unwrap();

    let result = engine
        .sql(
            "SELECT window_outer.n,
                    row_number() OVER (
                        PARTITION BY (
                            SELECT count(*) FROM window_inner
                            WHERE window_inner.x <= window_outer.n
                        )
                        ORDER BY window_outer.n
                    ) AS rn
             FROM window_outer
             ORDER BY window_outer.n",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.value_at(0, 0), Some(&uqa_core::Value::Int(1)));
    assert_eq!(result.value_at(0, 1), Some(&uqa_core::Value::Int(1)));
    assert_eq!(result.value_at(1, 0), Some(&uqa_core::Value::Int(3)));
    assert_eq!(result.value_at(1, 1), Some(&uqa_core::Value::Int(1)));
}

#[test]
fn correlated_subquery_in_group_key_uses_the_physical_outer_row() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE group_outer (id INTEGER)", &[])
        .unwrap();
    engine
        .sql("CREATE TABLE group_inner (id INTEGER)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO group_outer VALUES (1), (3)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO group_inner VALUES (1), (2), (3)", &[])
        .unwrap();

    let result = engine
        .sql(
            "SELECT count(*) AS c
             FROM group_outer
             GROUP BY (
                 SELECT count(*) FROM group_inner
                 WHERE group_inner.id <= group_outer.id
             )
             ORDER BY c",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.value_at(0, 0), Some(&uqa_core::Value::Int(1)));
    assert_eq!(result.value_at(1, 0), Some(&uqa_core::Value::Int(1)));
}
