//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `UPDATE t SET ... FROM other WHERE ...` and
//! `DELETE FROM t USING other WHERE ...` shapes. Matches UQA
//! reference's `_compile_update_from` / `_compile_delete_using`.

use uqa_core::Value;
use uqa_engine::Engine;

fn setup() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER, owner_id INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, balance, owner_id) VALUES \
         (1, 100, 10), (2, 200, 20), (3, 50, 30)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE bonuses (id INTEGER PRIMARY KEY, amount INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO bonuses (id, amount) VALUES (10, 5), (20, 10)",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn update_from_applies_per_join_match() {
    let eng = setup();
    let r = eng
        .sql(
            "UPDATE accounts SET balance = balance + amount FROM bonuses \
             WHERE accounts.owner_id = bonuses.id",
            &[],
        )
        .unwrap();
    assert_eq!(r.affected_rows, 2);
    let row1 = eng.get_document("accounts", 1).unwrap();
    assert_eq!(row1.get("balance"), Some(&Value::Int(105)));
    let row2 = eng.get_document("accounts", 2).unwrap();
    assert_eq!(row2.get("balance"), Some(&Value::Int(210)));
    // owner_id 30 has no matching bonus -> balance stays at 50.
    let row3 = eng.get_document("accounts", 3).unwrap();
    assert_eq!(row3.get("balance"), Some(&Value::Int(50)));
}

#[test]
fn delete_using_removes_matching_rows() {
    let eng = setup();
    let r = eng
        .sql(
            "DELETE FROM accounts USING bonuses \
             WHERE accounts.owner_id = bonuses.id",
            &[],
        )
        .unwrap();
    assert_eq!(r.affected_rows, 2);
    assert!(eng.get_document("accounts", 1).is_none());
    assert!(eng.get_document("accounts", 2).is_none());
    // owner_id 30 has no bonus -> row stays.
    assert!(eng.get_document("accounts", 3).is_some());
}
