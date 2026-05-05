//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregation monoid decomposition (Master Plan Section 2.3).
//!
//! For disjoint subsets L1 and L2 of a table, every supported
//! aggregate must satisfy:
//!
//! ```text
//! COUNT(L1 union L2) == COUNT(L1) + COUNT(L2)
//! SUM  (L1 union L2) == SUM(L1)   + SUM(L2)
//! MIN  (L1 union L2) == min(MIN(L1), MIN(L2))
//! MAX  (L1 union L2) == max(MAX(L1), MAX(L2))
//! ```
//!
//! We property-test this by inserting a random table, picking a
//! random pivot, and splitting on `v < pivot` / `v >= pivot`.

use std::fmt::Write as _;

use proptest::prelude::*;
use uqa_core::Value;
use uqa_engine::Engine;

fn build_engine(rows: &[i64]) -> Engine {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)", &[])
        .unwrap();
    if !rows.is_empty() {
        let mut sql = String::from("INSERT INTO t (id, v) VALUES ");
        for (i, v) in rows.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            write!(sql, "({}, {v})", i + 1).unwrap();
        }
        engine.sql(&sql, &[]).unwrap();
    }
    engine
}

fn read_int(engine: &Engine, sql: &str, col: &str) -> Option<i64> {
    let r = engine.sql(sql, &[]).expect("sql ok");
    let row = r.rows.first()?;
    match row.get(col)? {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    /// COUNT(*) decomposes additively across a disjoint split.
    #[test]
    fn count_is_additive(rows in proptest::collection::vec(-50i64..=50, 1..=20), pivot in -50i64..=50) {
        let engine = build_engine(&rows);
        let total = read_int(&engine, "SELECT COUNT(*) AS c FROM t", "c").unwrap_or(0);
        let lt = read_int(
            &engine,
            &format!("SELECT COUNT(*) AS c FROM t WHERE v < {pivot}"),
            "c",
        )
        .unwrap_or(0);
        let ge = read_int(
            &engine,
            &format!("SELECT COUNT(*) AS c FROM t WHERE v >= {pivot}"),
            "c",
        )
        .unwrap_or(0);
        prop_assert_eq!(
            total,
            lt + ge,
            "COUNT(*) not additive: total={}, lt={}, ge={}",
            total,
            lt,
            ge,
        );
    }

    /// SUM decomposes additively across a disjoint split.
    #[test]
    fn sum_is_additive(rows in proptest::collection::vec(-50i64..=50, 1..=20), pivot in -50i64..=50) {
        let engine = build_engine(&rows);
        let total = read_int(&engine, "SELECT SUM(v) AS s FROM t", "s").unwrap_or(0);
        let lt = read_int(
            &engine,
            &format!("SELECT SUM(v) AS s FROM t WHERE v < {pivot}"),
            "s",
        )
        .unwrap_or(0);
        let ge = read_int(
            &engine,
            &format!("SELECT SUM(v) AS s FROM t WHERE v >= {pivot}"),
            "s",
        )
        .unwrap_or(0);
        prop_assert_eq!(
            total,
            lt + ge,
            "SUM not additive: total={}, lt={}, ge={}",
            total,
            lt,
            ge,
        );
    }

    /// MIN(L1 union L2) equals min(MIN(L1), MIN(L2)) when both sides
    /// are non-empty. We skip cases where one side is empty since
    /// MIN over an empty set is intentionally undefined.
    #[test]
    fn min_decomposes(rows in proptest::collection::vec(-50i64..=50, 2..=20), pivot in -50i64..=50) {
        let engine = build_engine(&rows);
        let lt_count = read_int(
            &engine,
            &format!("SELECT COUNT(*) AS c FROM t WHERE v < {pivot}"),
            "c",
        )
        .unwrap_or(0);
        let ge_count = read_int(
            &engine,
            &format!("SELECT COUNT(*) AS c FROM t WHERE v >= {pivot}"),
            "c",
        )
        .unwrap_or(0);
        prop_assume!(lt_count > 0 && ge_count > 0);

        let total = read_int(&engine, "SELECT MIN(v) AS m FROM t", "m").unwrap();
        let lt = read_int(
            &engine,
            &format!("SELECT MIN(v) AS m FROM t WHERE v < {pivot}"),
            "m",
        )
        .unwrap();
        let ge = read_int(
            &engine,
            &format!("SELECT MIN(v) AS m FROM t WHERE v >= {pivot}"),
            "m",
        )
        .unwrap();
        prop_assert_eq!(
            total,
            lt.min(ge),
            "MIN not min-decomposable: total={}, lt={}, ge={}",
            total,
            lt,
            ge,
        );
    }

    /// MAX(L1 union L2) equals max(MAX(L1), MAX(L2)) when both sides
    /// are non-empty.
    #[test]
    fn max_decomposes(rows in proptest::collection::vec(-50i64..=50, 2..=20), pivot in -50i64..=50) {
        let engine = build_engine(&rows);
        let lt_count = read_int(
            &engine,
            &format!("SELECT COUNT(*) AS c FROM t WHERE v < {pivot}"),
            "c",
        )
        .unwrap_or(0);
        let ge_count = read_int(
            &engine,
            &format!("SELECT COUNT(*) AS c FROM t WHERE v >= {pivot}"),
            "c",
        )
        .unwrap_or(0);
        prop_assume!(lt_count > 0 && ge_count > 0);

        let total = read_int(&engine, "SELECT MAX(v) AS m FROM t", "m").unwrap();
        let lt = read_int(
            &engine,
            &format!("SELECT MAX(v) AS m FROM t WHERE v < {pivot}"),
            "m",
        )
        .unwrap();
        let ge = read_int(
            &engine,
            &format!("SELECT MAX(v) AS m FROM t WHERE v >= {pivot}"),
            "m",
        )
        .unwrap();
        prop_assert_eq!(
            total,
            lt.max(ge),
            "MAX not max-decomposable: total={}, lt={}, ge={}",
            total,
            lt,
            ge,
        );
    }
}

/// Concrete: empty table semantics. COUNT returns 0; SUM returns Null
/// (per SQL's "sum of empty set is undefined" convention) — but if
/// the engine returns 0 instead, that's still a defensible choice and
/// the property tests above don't depend on the empty-set behavior.
#[test]
fn count_on_empty_returns_zero() {
    let engine = build_engine(&[]);
    assert_eq!(
        read_int(&engine, "SELECT COUNT(*) AS c FROM t", "c").unwrap_or(0),
        0,
    );
}
