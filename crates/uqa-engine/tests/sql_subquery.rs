//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Subquery shapes: scalar `(SELECT ...)`, `EXISTS (...)`, and
//! `IN (SELECT ...)` coverage.

use uqa_core::Value;
use uqa_engine::Engine;

fn setup() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER, owner TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, balance, owner) VALUES \
         (1, 100, 'alice'), (2, 200, 'bob'), (3, 50, 'carol')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE owners (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO owners (id, name) VALUES (1, 'alice'), (2, 'bob')",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn scalar_subquery_in_projection() {
    let eng = setup();
    let r = eng
        .sql(
            "SELECT (SELECT count(*) FROM accounts WHERE balance >= 100) AS n",
            &[],
        )
        .unwrap();
    let n = r.rows[0].get("n").cloned().unwrap_or(Value::Null);
    // count(*) returns Int 2 (alice + bob).
    assert_eq!(n, Value::Int(2));
}

#[test]
fn in_subquery_filters_rows() {
    let eng = setup();
    let r = eng
        .sql(
            "SELECT id FROM accounts WHERE owner IN (SELECT name FROM owners) ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(i)) => Some(*i),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn exists_filters_when_any_row() {
    let eng = setup();
    let r = eng
        .sql(
            "SELECT id FROM accounts WHERE EXISTS (SELECT 1 FROM owners WHERE name = 'alice')",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 3);
}

#[test]
fn exists_filters_when_no_rows() {
    let eng = setup();
    let r = eng
        .sql(
            "SELECT id FROM accounts WHERE EXISTS (SELECT 1 FROM owners WHERE name = 'zzz')",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 0);
}
