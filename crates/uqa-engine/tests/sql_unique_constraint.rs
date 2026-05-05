//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UNIQUE constraint validation. Mirrors the Python reference's
//! per-row UNIQUE check before INSERT.

use uqa_engine::Engine;

#[test]
fn unique_constraint_rejects_duplicate_value() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, email, name) VALUES (1, 'a@x.com', 'alice')",
        &[],
    )
    .unwrap();
    let err = eng
        .sql(
            "INSERT INTO accounts (id, email, name) VALUES (2, 'a@x.com', 'alice2')",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.to_ascii_lowercase().contains("unique"),
        "expected UNIQUE error, got {msg}"
    );
}

#[test]
fn unique_constraint_allows_distinct_values() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, email, name) VALUES (1, 'a@x.com', 'alice'), (2, 'b@x.com', 'bob')",
        &[],
    )
    .unwrap();
    let one = eng.get_document("accounts", 1).unwrap();
    let two = eng.get_document("accounts", 2).unwrap();
    assert_ne!(one.get("email"), two.get("email"));
}
