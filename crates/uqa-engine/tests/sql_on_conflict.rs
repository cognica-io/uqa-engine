//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ON CONFLICT` (UPSERT) coverage. Exercises both DO NOTHING and DO
//! UPDATE branches against a small in-memory table and checks that
//! the conflict target column drives the merge decision.

use uqa_core::Value;
use uqa_engine::Engine;

fn setup() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT, balance INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, name, balance) VALUES (1, 'alice', 100), (2, 'bob', 50)",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn on_conflict_do_nothing_skips_existing_row() {
    let eng = setup();
    let result = eng
        .sql(
            "INSERT INTO accounts (id, name, balance) VALUES (1, 'alice2', 999) \
             ON CONFLICT (id) DO NOTHING",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 0);
    let row = eng.get_document("accounts", 1).unwrap();
    assert_eq!(row.get("name"), Some(&Value::Str("alice".into())));
    assert_eq!(row.get("balance"), Some(&Value::Int(100)));
}

#[test]
fn on_conflict_do_update_applies_assignments() {
    let eng = setup();
    let result = eng
        .sql(
            "INSERT INTO accounts (id, name, balance) VALUES (1, 'alice', 200) \
             ON CONFLICT (id) DO UPDATE SET balance = 200",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 1);
    let row = eng.get_document("accounts", 1).unwrap();
    assert_eq!(row.get("balance"), Some(&Value::Int(200)));
    // The non-targeted column stays the same.
    assert_eq!(row.get("name"), Some(&Value::Str("alice".into())));
}

#[test]
fn on_conflict_falls_through_to_insert_when_no_match() {
    let eng = setup();
    let result = eng
        .sql(
            "INSERT INTO accounts (id, name, balance) VALUES (3, 'carol', 75) \
             ON CONFLICT (id) DO NOTHING",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 1);
    let row = eng.get_document("accounts", 3).unwrap();
    assert_eq!(row.get("name"), Some(&Value::Str("carol".into())));
}
