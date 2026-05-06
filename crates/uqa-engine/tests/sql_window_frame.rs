//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window FRAME (ROWS/RANGE BETWEEN) over aggregate window
//! functions. Mirrors Python `_compute_framed_aggregate` semantics.

use uqa_core::Value;
use uqa_engine::Engine;

fn setup() -> Engine {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO t (id, n) VALUES (1, 10), (2, 20), (3, 30), (4, 40), (5, 50)",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn rows_unbounded_preceding_to_current_row_is_running_sum() {
    let eng = setup();
    let r = eng
        .sql(
            "SELECT id, SUM(n) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS s FROM t ORDER BY id",
            &[],
        )
        .unwrap();
    let s: Vec<f64> = r
        .rows
        .iter()
        .map(|row| match row.get("s") {
            Some(Value::Float(f)) => *f,
            Some(Value::Int(i)) => *i as f64,
            other => panic!("unexpected: {other:?}"),
        })
        .collect();
    assert_eq!(s, vec![10.0, 30.0, 60.0, 100.0, 150.0]);
}

#[test]
fn rows_n_preceding_to_current_row() {
    let eng = setup();
    let r = eng
        .sql(
            "SELECT id, SUM(n) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS s FROM t ORDER BY id",
            &[],
        )
        .unwrap();
    let s: Vec<f64> = r
        .rows
        .iter()
        .map(|row| match row.get("s") {
            Some(Value::Float(f)) => *f,
            Some(Value::Int(i)) => *i as f64,
            other => panic!("unexpected: {other:?}"),
        })
        .collect();
    assert_eq!(s, vec![10.0, 30.0, 50.0, 70.0, 90.0]);
}

#[test]
fn default_frame_when_order_by_present_is_running_aggregate() {
    let eng = setup();
    let r = eng
        .sql(
            "SELECT id, SUM(n) OVER (ORDER BY id) AS s FROM t ORDER BY id",
            &[],
        )
        .unwrap();
    let s: Vec<f64> = r
        .rows
        .iter()
        .map(|row| match row.get("s") {
            Some(Value::Float(f)) => *f,
            Some(Value::Int(i)) => *i as f64,
            other => panic!("unexpected: {other:?}"),
        })
        .collect();
    assert_eq!(s, vec![10.0, 30.0, 60.0, 100.0, 150.0]);
}

#[test]
fn no_order_by_aggregates_whole_partition() {
    let eng = setup();
    let r = eng
        .sql("SELECT id, SUM(n) OVER () AS s FROM t ORDER BY id", &[])
        .unwrap();
    let s: Vec<f64> = r
        .rows
        .iter()
        .map(|row| match row.get("s") {
            Some(Value::Float(f)) => *f,
            Some(Value::Int(i)) => *i as f64,
            other => panic!("unexpected: {other:?}"),
        })
        .collect();
    assert_eq!(s, vec![150.0; 5]);
}
