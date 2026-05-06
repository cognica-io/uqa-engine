//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ORDER BY ... NULLS FIRST/LAST`. Mirrors `PostgreSQL` semantics:
//! ASC defaults to NULLS LAST and DESC defaults to NULLS FIRST; an
//! explicit clause overrides the default.

use uqa_core::Value;
use uqa_engine::Engine;

fn setup() -> Engine {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO t (id, n) VALUES (1, 10), (2, NULL), (3, 5), (4, NULL), (5, 20)",
        &[],
    )
    .unwrap();
    eng
}

fn id_order(rows: &[std::collections::BTreeMap<String, Value>]) -> Vec<i64> {
    rows.iter()
        .map(|r| match r.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect()
}

#[test]
fn asc_default_places_nulls_last() {
    let eng = setup();
    let r = eng.sql("SELECT id FROM t ORDER BY n ASC", &[]).unwrap();
    let order = id_order(&r.rows);
    let null_positions: Vec<usize> = order
        .iter()
        .enumerate()
        .filter(|(_, id)| **id == 2 || **id == 4)
        .map(|(i, _)| i)
        .collect();
    assert!(null_positions.iter().all(|p| *p >= 3));
}

#[test]
fn asc_nulls_first_places_nulls_first() {
    let eng = setup();
    let r = eng
        .sql("SELECT id FROM t ORDER BY n ASC NULLS FIRST", &[])
        .unwrap();
    let order = id_order(&r.rows);
    assert!(order[0] == 2 || order[0] == 4);
    assert!(order[1] == 2 || order[1] == 4);
}

#[test]
fn desc_default_places_nulls_first() {
    let eng = setup();
    let r = eng.sql("SELECT id FROM t ORDER BY n DESC", &[]).unwrap();
    let order = id_order(&r.rows);
    assert!(order[0] == 2 || order[0] == 4);
    assert!(order[1] == 2 || order[1] == 4);
}

#[test]
fn desc_nulls_last_places_nulls_last() {
    let eng = setup();
    let r = eng
        .sql("SELECT id FROM t ORDER BY n DESC NULLS LAST", &[])
        .unwrap();
    let order = id_order(&r.rows);
    let last_two = &order[order.len() - 2..];
    assert!(last_two.contains(&2));
    assert!(last_two.contains(&4));
}
