//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` operator-desugaring equivalence tests.
//!
//! The compiler rewrites `~~`, `~~*`, `!~~`, `!~~*` into `LIKE` /
//! `ILIKE` / `NOT LIKE` / `NOT ILIKE`. The rewrite must be
//! semantics-preserving: a SELECT using the operator form must
//! return the exact same rows as the one using the keyword form.

use uqa_core::Value;
use uqa_engine::Engine;

fn corpus() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO notes (id, body) VALUES \
             (1, 'Apple'), (2, 'apple'), (3, 'banana'), (4, 'APPLE PIE'), (5, 'Cherry')",
            &[],
        )
        .unwrap();
    engine
}

fn collect_ids(engine: &Engine, sql: &str) -> Vec<i64> {
    engine
        .sql(sql, &[])
        .expect("sql ok")
        .rows
        .iter()
        .filter_map(|row| match row.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect()
}

#[test]
fn tilde_tilde_matches_like() {
    let engine = corpus();
    let with_op = collect_ids(
        &engine,
        "SELECT id FROM notes WHERE body ~~ 'app%' ORDER BY id",
    );
    let with_keyword = collect_ids(
        &engine,
        "SELECT id FROM notes WHERE body LIKE 'app%' ORDER BY id",
    );
    assert_eq!(with_op, with_keyword);
}

#[test]
fn tilde_tilde_star_matches_ilike() {
    let engine = corpus();
    let with_op = collect_ids(
        &engine,
        "SELECT id FROM notes WHERE body ~~* 'app%' ORDER BY id",
    );
    let with_keyword = collect_ids(
        &engine,
        "SELECT id FROM notes WHERE body ILIKE 'app%' ORDER BY id",
    );
    assert_eq!(with_op, with_keyword);
}

#[test]
fn bang_tilde_tilde_matches_not_like() {
    let engine = corpus();
    let with_op = collect_ids(
        &engine,
        "SELECT id FROM notes WHERE body !~~ 'app%' ORDER BY id",
    );
    let with_keyword = collect_ids(
        &engine,
        "SELECT id FROM notes WHERE body NOT LIKE 'app%' ORDER BY id",
    );
    assert_eq!(with_op, with_keyword);
}

#[test]
fn bang_tilde_tilde_star_matches_not_ilike() {
    let engine = corpus();
    let with_op = collect_ids(
        &engine,
        "SELECT id FROM notes WHERE body !~~* 'app%' ORDER BY id",
    );
    let with_keyword = collect_ids(
        &engine,
        "SELECT id FROM notes WHERE body NOT ILIKE 'app%' ORDER BY id",
    );
    assert_eq!(with_op, with_keyword);
}

/// `~~` and `!~~` are exact complements: every row that matches one
/// must not match the other, and vice versa. This is the de-Morgan
/// dual at the operator level.
#[test]
fn op_and_negated_op_partition_the_table() {
    let engine = corpus();
    let positive = collect_ids(
        &engine,
        "SELECT id FROM notes WHERE body ~~ 'app%' ORDER BY id",
    );
    let negative = collect_ids(
        &engine,
        "SELECT id FROM notes WHERE body !~~ 'app%' ORDER BY id",
    );
    let all = collect_ids(&engine, "SELECT id FROM notes ORDER BY id");
    let mut union: Vec<i64> = positive.iter().chain(negative.iter()).copied().collect();
    union.sort_unstable();
    union.dedup();
    assert_eq!(union, all);
    // Disjoint: no id appears in both sides.
    let intersect: Vec<i64> = positive
        .iter()
        .filter(|id| negative.contains(id))
        .copied()
        .collect();
    assert!(intersect.is_empty(), "overlap: {intersect:?}");
}

/// `||` is left-associative. Both groupings produce the same string.
#[test]
fn string_concat_is_associative() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    let left = engine
        .sql("SELECT (('a' || 'b') || 'c') AS r FROM t", &[])
        .unwrap();
    let right = engine
        .sql("SELECT ('a' || ('b' || 'c')) AS r FROM t", &[])
        .unwrap();
    assert_eq!(
        left.rows.first().and_then(|r| r.get("r")),
        right.rows.first().and_then(|r| r.get("r")),
    );
    assert_eq!(
        left.rows.first().and_then(|r| r.get("r")),
        Some(&Value::Str("abc".into())),
    );
}

/// `||` with empty-string identity: `x || ''` and `'' || x` both
/// equal `x`.
#[test]
fn string_concat_has_empty_identity() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    let left = engine.sql("SELECT 'hello' || '' AS r FROM t", &[]).unwrap();
    let right = engine.sql("SELECT '' || 'hello' AS r FROM t", &[]).unwrap();
    assert_eq!(
        left.rows.first().and_then(|r| r.get("r")),
        Some(&Value::Str("hello".into())),
    );
    assert_eq!(
        right.rows.first().and_then(|r| r.get("r")),
        Some(&Value::Str("hello".into())),
    );
}
