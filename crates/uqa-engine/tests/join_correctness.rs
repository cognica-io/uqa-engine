//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hash-join correctness property test.
//!
//! `try_hash_inner_join` rewrites equijoins from a nested-loop scan to
//! a bucketed probe (~340x speedup on the engine join bench). The
//! rewrite must be semantics-preserving, i.e. the optimized engine
//! result must equal the pure-Rust nested-loop oracle for any random
//! pair of input tables.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use proptest::prelude::*;
use uqa_core::Value;
use uqa_engine::Engine;

/// One left-side row.
type LeftRow = (i64, i64, String);

/// One right-side row.
type RightRow = (i64, i64, String);

fn build_engine(left: &[LeftRow], right: &[RightRow]) -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE l (id INTEGER PRIMARY KEY, k INTEGER, label TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE r (id INTEGER PRIMARY KEY, k INTEGER, tag TEXT)",
            &[],
        )
        .unwrap();
    if !left.is_empty() {
        let mut sql = String::from("INSERT INTO l (id, k, label) VALUES ");
        for (i, (id, k, label)) in left.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            write!(sql, "({id}, {k}, '{}')", label.replace('\'', "''")).unwrap();
        }
        engine.sql(&sql, &[]).unwrap();
    }
    if !right.is_empty() {
        let mut sql = String::from("INSERT INTO r (id, k, tag) VALUES ");
        for (i, (rid, k, tag)) in right.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            write!(sql, "({rid}, {k}, '{}')", tag.replace('\'', "''")).unwrap();
        }
        engine.sql(&sql, &[]).unwrap();
    }
    engine
}

/// Pure-Rust nested-loop oracle. Returns rows as
/// `(id, l.k, label, rid, r.k, tag)` tuples sorted lexicographically.
fn oracle(left: &[LeftRow], right: &[RightRow]) -> Vec<(i64, i64, String, i64, i64, String)> {
    let mut out = Vec::new();
    for (id, lk, label) in left {
        for (rid, rk, tag) in right {
            if lk == rk {
                out.push((*id, *lk, label.clone(), *rid, *rk, tag.clone()));
            }
        }
    }
    out.sort();
    out
}

/// Pulls the same six columns out of an engine `SELECT *` result. We
/// rely on the engine prefixing right-side columns with the right
/// table's alias when a name collision occurs (`l.k`, `r.k`); the
/// test asks for explicit aliases via the SELECT list to avoid
/// guessing what the engine names the duplicate `k`.
fn engine_join(engine: &Engine) -> Vec<(i64, i64, String, i64, i64, String)> {
    let r = engine
        .sql(
            "SELECT l.id AS id, l.k AS lk, l.label AS label, \
                    r.id AS rid, r.k AS rk, r.tag AS tag \
             FROM l INNER JOIN r ON l.k = r.k",
            &[],
        )
        .expect("join sql");
    let mut rows = Vec::with_capacity(r.rows.len());
    for row in &r.rows {
        let id = match row.get("id") {
            Some(Value::Int(n)) => *n,
            _ => continue,
        };
        let lk = match row.get("lk") {
            Some(Value::Int(n)) => *n,
            _ => continue,
        };
        let label = match row.get("label") {
            Some(Value::Str(s)) => s.clone(),
            _ => continue,
        };
        let rid = match row.get("rid") {
            Some(Value::Int(n)) => *n,
            _ => continue,
        };
        let rk = match row.get("rk") {
            Some(Value::Int(n)) => *n,
            _ => continue,
        };
        let tag = match row.get("tag") {
            Some(Value::Str(s)) => s.clone(),
            _ => continue,
        };
        rows.push((id, lk, label, rid, rk, tag));
    }
    rows.sort();
    rows
}

/// Strategy: independent left and right tables drawn from a small
/// shared key universe so the join produces non-trivial overlap. Keys
/// are sometimes duplicated within a side too, which exercises the
/// bucketing branch where one bucket carries multiple rows.
fn arb_pair() -> impl Strategy<Value = (Vec<LeftRow>, Vec<RightRow>)> {
    let left =
        proptest::collection::vec((1i64..=100, 0i64..=8, "[a-z]{1,4}"), 0..=12).prop_map(|rows| {
            let mut seen = BTreeMap::new();
            for (id, k, label) in rows {
                seen.insert(id, (k, label));
            }
            seen.into_iter()
                .map(|(id, (k, label))| (id, k, label))
                .collect::<Vec<_>>()
        });
    let right =
        proptest::collection::vec((1i64..=100, 0i64..=8, "[A-Z]{1,4}"), 0..=12).prop_map(|rows| {
            let mut seen = BTreeMap::new();
            for (id, k, tag) in rows {
                seen.insert(id, (k, tag));
            }
            seen.into_iter()
                .map(|(rid, (k, tag))| (rid, k, tag))
                .collect::<Vec<_>>()
        });
    (left, right)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        ..ProptestConfig::default()
    })]

    /// Engine-level INNER JOIN result must match the nested-loop oracle
    /// for any input pair. Catches a regression in either the
    /// hash-join optimizer or the fallback path.
    #[test]
    fn inner_join_matches_oracle((left, right) in arb_pair()) {
        let engine = build_engine(&left, &right);
        let observed = engine_join(&engine);
        let expected = oracle(&left, &right);
        prop_assert_eq!(observed, expected);
    }
}
