//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `UPDATE t SET ... FROM other WHERE ...` and
//! `DELETE FROM t USING other WHERE ...` shapes.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ColumnType;

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
    let row1 = eng
        .get_document("accounts", 1)
        .unwrap()
        .expect("updated row 1");
    assert_eq!(row1.get("balance"), Some(&Value::Int(105)));
    let row2 = eng
        .get_document("accounts", 2)
        .unwrap()
        .expect("updated row 2");
    assert_eq!(row2.get("balance"), Some(&Value::Int(210)));
    // owner_id 30 has no matching bonus -> balance stays at 50.
    let row3 = eng
        .get_document("accounts", 3)
        .unwrap()
        .expect("unmatched row 3");
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
    assert!(eng.get_document("accounts", 1).unwrap().is_none());
    assert!(eng.get_document("accounts", 2).unwrap().is_none());
    // owner_id 30 has no bonus -> row stays.
    assert!(eng.get_document("accounts", 3).unwrap().is_some());
}

#[test]
fn returning_binds_and_evaluates_from_and_using_columns() {
    let eng = setup();
    let updated = eng
        .sql(
            "UPDATE accounts SET balance = balance + amount FROM bonuses
             WHERE accounts.owner_id = bonuses.id
             RETURNING accounts.id AS account_id, bonuses.amount AS applied_amount",
            &[],
        )
        .unwrap();
    assert_eq!(updated.rows.len(), 2);
    assert_eq!(
        updated.column_types,
        [Some(ColumnType::Integer), Some(ColumnType::Integer)]
    );
    assert!(updated
        .rows
        .iter()
        .any(|row| row.get("applied_amount") == Some(&Value::Int(5))));

    let deleted = eng
        .sql(
            "DELETE FROM accounts USING bonuses
             WHERE accounts.owner_id = bonuses.id AND bonuses.amount = 10
             RETURNING accounts.id AS account_id, bonuses.amount AS applied_amount",
            &[],
        )
        .unwrap();
    assert_eq!(deleted.rows.len(), 1);
    assert_eq!(deleted.rows[0]["account_id"], Value::Int(2));
    assert_eq!(deleted.rows[0]["applied_amount"], Value::Int(10));
    assert_eq!(
        deleted.column_types,
        [Some(ColumnType::Integer), Some(ColumnType::Integer)]
    );
}

#[test]
fn delete_using_empty_source_matches_no_target_rows() {
    let eng = setup();
    eng.sql("CREATE TABLE empty_keys (id INTEGER)", &[])
        .unwrap();
    let deleted = eng
        .sql(
            "DELETE FROM accounts USING empty_keys RETURNING accounts.id",
            &[],
        )
        .unwrap();
    assert_eq!(deleted.affected_rows, 0);
    assert!(deleted.rows.is_empty());
    assert_eq!(deleted.column_types, [Some(ColumnType::Integer)]);
    let remaining = eng
        .sql("SELECT count(*) AS count FROM accounts", &[])
        .unwrap();
    assert_eq!(remaining.rows[0]["count"], Value::Int(3));
}
