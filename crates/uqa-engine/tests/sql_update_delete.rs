//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE and DELETE round-trips with WHERE filtering.

use uqa_core::Value;
use uqa_engine::Engine;

fn corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT, qty INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO items (id, label, qty) VALUES \
         (1, 'apple', 3), (2, 'banana', 7), (3, 'cherry', 2), (4, 'date', 5)",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn update_changes_matched_rows() {
    let eng = corpus();
    let r = eng
        .sql("UPDATE items SET qty = 99 WHERE label = 'banana'", &[])
        .unwrap();
    assert_eq!(r.affected_rows, 1);
    let r = eng
        .sql("SELECT label, qty FROM items WHERE label = 'banana'", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].get("qty"), Some(&Value::Int(99)));
}

#[test]
fn update_with_arithmetic_expression() {
    let eng = corpus();
    let r = eng
        .sql("UPDATE items SET qty = qty + 1 WHERE qty < 5", &[])
        .unwrap();
    assert_eq!(r.affected_rows, 2); // apple, cherry

    let r = eng
        .sql("SELECT label, qty FROM items ORDER BY label", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 4);
    let map: std::collections::BTreeMap<_, _> = r
        .rows
        .iter()
        .filter_map(|row| match (row.get("label"), row.get("qty")) {
            (Some(Value::Str(l)), Some(Value::Int(n))) => Some((l.clone(), *n)),
            _ => None,
        })
        .collect();
    assert_eq!(map.get("apple"), Some(&4));
    assert_eq!(map.get("cherry"), Some(&3));
    assert_eq!(map.get("banana"), Some(&7));
    assert_eq!(map.get("date"), Some(&5));
}

#[test]
fn delete_removes_matched_rows() {
    let eng = corpus();
    let r = eng.sql("DELETE FROM items WHERE qty <= 3", &[]).unwrap();
    assert_eq!(r.affected_rows, 2);
    let r = eng.sql("SELECT count(*) AS n FROM items", &[]).unwrap();
    assert_eq!(r.rows[0].get("n"), Some(&Value::Int(2)));
}

#[test]
fn delete_without_where_truncates_table() {
    let eng = corpus();
    let r = eng.sql("DELETE FROM items", &[]).unwrap();
    assert_eq!(r.affected_rows, 4);
    let r = eng.sql("SELECT count(*) AS n FROM items", &[]).unwrap();
    assert_eq!(r.rows[0].get("n"), Some(&Value::Int(0)));
}
