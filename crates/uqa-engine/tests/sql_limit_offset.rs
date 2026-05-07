//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end coverage for `LIMIT` / `OFFSET` across every SELECT
//! shape: bare projection, ORDER BY by source column, parameterised
//! LIMIT / OFFSET, GROUP BY, JOIN, set operations, CTE, and the
//! search-aware `_score` ordering path. Mirrors the matrix Python
//! `uqa.sql.compiler` exercises in its own test suite.

use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

fn engine() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT, qty INTEGER)",
        &[],
    )
    .unwrap();
    let bodies = ["alpha", "beta", "gamma", "delta", "epsilon"];
    for (i, b) in bodies.iter().enumerate() {
        eng.sql(
            "INSERT INTO notes (body, qty) VALUES ($1, $2)",
            &[
                SQLParam::scalar(Value::Str((*b).into())),
                SQLParam::scalar(Value::Int(10 * (i as i64 + 1))),
            ],
        )
        .unwrap();
    }
    eng
}

fn ids(eng: &Engine, sql: &str, params: &[SQLParam]) -> Vec<i64> {
    eng.sql(sql, params)
        .unwrap()
        .rows
        .iter()
        .map(|r| match r.get("id").unwrap() {
            Value::Int(n) => *n,
            other => panic!("non-int id: {other:?}"),
        })
        .collect()
}

#[test]
fn plain_limit_no_order_truncates() {
    let eng = engine();
    assert_eq!(ids(&eng, "SELECT id FROM notes LIMIT 2", &[]).len(), 2);
}

#[test]
fn limit_zero_returns_empty() {
    let eng = engine();
    assert!(ids(&eng, "SELECT id FROM notes LIMIT 0", &[]).is_empty());
}

#[test]
fn limit_larger_than_row_count_returns_all() {
    let eng = engine();
    assert_eq!(ids(&eng, "SELECT id FROM notes LIMIT 99", &[]).len(), 5);
}

#[test]
fn offset_alone_skips() {
    let eng = engine();
    let r = ids(&eng, "SELECT id FROM notes ORDER BY id OFFSET 2", &[]);
    assert_eq!(r, vec![3, 4, 5]);
}

#[test]
fn offset_larger_than_row_count_empty() {
    let eng = engine();
    assert!(ids(&eng, "SELECT id FROM notes OFFSET 99", &[]).is_empty());
}

#[test]
fn limit_with_offset_combines() {
    let eng = engine();
    let r = ids(
        &eng,
        "SELECT id FROM notes ORDER BY id LIMIT 2 OFFSET 1",
        &[],
    );
    assert_eq!(r, vec![2, 3]);
}

#[test]
fn order_by_source_column_after_limit() {
    let eng = engine();
    // ORDER BY references `qty` which the projection drops; PG
    // semantics keep the column visible to ORDER BY.
    let r = ids(&eng, "SELECT id FROM notes ORDER BY qty DESC LIMIT 2", &[]);
    assert_eq!(r, vec![5, 4]);
}

#[test]
fn order_by_qty_asc_offset_skips_smallest() {
    let eng = engine();
    let r = ids(
        &eng,
        "SELECT id FROM notes ORDER BY qty ASC LIMIT 2 OFFSET 2",
        &[],
    );
    assert_eq!(r, vec![3, 4]);
}

#[test]
fn parameterised_limit_offset_binds() {
    let eng = engine();
    let r = ids(
        &eng,
        "SELECT id FROM notes ORDER BY id LIMIT $1 OFFSET $2",
        &[
            SQLParam::scalar(Value::Int(2)),
            SQLParam::scalar(Value::Int(1)),
        ],
    );
    assert_eq!(r, vec![2, 3]);
}

#[test]
fn limit_with_text_match_score_order() {
    let eng = engine();
    eng.sql("CREATE INDEX notes_body_idx ON notes USING gin (body)", &[])
        .unwrap();
    let r = eng
        .sql(
            "SELECT id, _score FROM notes WHERE text_match(body, 'alpha') \
             ORDER BY _score DESC LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
}

#[test]
fn limit_after_group_by() {
    let eng = engine();
    let r = eng
        .sql(
            "SELECT qty, COUNT(*) AS n FROM notes GROUP BY qty ORDER BY qty LIMIT 2",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2);
    let q1 = match r.rows[0].get("qty").unwrap() {
        Value::Int(n) => *n,
        other => panic!("unexpected qty: {other:?}"),
    };
    let q2 = match r.rows[1].get("qty").unwrap() {
        Value::Int(n) => *n,
        other => panic!("unexpected qty: {other:?}"),
    };
    assert_eq!((q1, q2), (10, 20));
}

#[test]
fn outer_limit_applies_to_union_result() {
    let eng = engine();
    // PG semantics: trailing LIMIT applies to the combined UNION
    // result, not to the LHS branch alone.
    let r = ids(
        &eng,
        "SELECT id FROM notes WHERE qty <= 20 \
         UNION ALL \
         SELECT id FROM notes WHERE qty >= 40 \
         ORDER BY id LIMIT 3",
        &[],
    );
    assert_eq!(r, vec![1, 2, 4]);
}

#[test]
fn limit_inside_cte_is_honoured() {
    let eng = engine();
    let r = ids(
        &eng,
        "WITH top AS (SELECT id FROM notes ORDER BY qty DESC LIMIT 2) \
         SELECT id FROM top ORDER BY id",
        &[],
    );
    assert_eq!(r, vec![4, 5]);
}

#[test]
fn limit_with_join() {
    let eng = engine();
    eng.sql("CREATE TABLE meta (id BIGINT PRIMARY KEY, label TEXT)", &[])
        .unwrap();
    for i in 1..=5 {
        eng.sql(
            "INSERT INTO meta (id, label) VALUES ($1, $2)",
            &[
                SQLParam::scalar(Value::Int(i)),
                SQLParam::scalar(Value::Str(format!("L{i}"))),
            ],
        )
        .unwrap();
    }
    let r = eng
        .sql(
            "SELECT n.id FROM notes n JOIN meta m ON m.id = n.id \
             ORDER BY n.id LIMIT 2 OFFSET 1",
            &[],
        )
        .unwrap();
    let collected: Vec<i64> = r
        .rows
        .iter()
        .map(|r| match r.get("id").unwrap() {
            Value::Int(n) => *n,
            other => panic!("non-int id: {other:?}"),
        })
        .collect();
    assert_eq!(collected, vec![2, 3]);
}
