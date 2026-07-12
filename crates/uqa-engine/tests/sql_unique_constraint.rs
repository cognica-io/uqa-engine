//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UNIQUE constraint validation. Mirrors the canonical UQA behavior's
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

/// The UNIQUE probe on insert resolves through the column value index
/// (built lazily by the first probe, maintained incrementally after):
/// duplicates of the first, a middle, and the last inserted value must
/// all still be rejected once hundreds of rows have flowed through the
/// incremental maintenance path, and a fresh value must still insert.
#[test]
fn unique_check_stays_correct_through_the_value_index() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE)",
        &[],
    )
    .unwrap();
    for i in 0..500 {
        eng.sql(
            &format!("INSERT INTO accounts (id, email) VALUES ({i}, 'u{i}@x.com')"),
            &[],
        )
        .unwrap();
    }
    for dup in ["u0@x.com", "u250@x.com", "u499@x.com"] {
        let err = eng
            .sql(
                &format!("INSERT INTO accounts (id, email) VALUES (1000, '{dup}')"),
                &[],
            )
            .unwrap_err();
        let msg = format!("{err:?}").to_ascii_lowercase();
        assert!(msg.contains("unique"), "expected UNIQUE error, got {msg}");
    }
    eng.sql(
        "INSERT INTO accounts (id, email) VALUES (1000, 'fresh@x.com')",
        &[],
    )
    .unwrap();
}

/// Deletes and updates must free a unique value for reuse and claim the
/// new one, through the same incrementally maintained index the insert
/// probe reads.
#[test]
fn unique_value_frees_on_delete_and_moves_on_update() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, email) VALUES (1, 'a@x.com'), (2, 'b@x.com')",
        &[],
    )
    .unwrap();
    eng.sql("DELETE FROM accounts WHERE id = 1", &[]).unwrap();
    eng.sql(
        "INSERT INTO accounts (id, email) VALUES (3, 'a@x.com')",
        &[],
    )
    .unwrap();
    eng.sql("UPDATE accounts SET email = 'c@x.com' WHERE id = 2", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, email) VALUES (4, 'b@x.com')",
        &[],
    )
    .unwrap();
    let err = eng
        .sql(
            "INSERT INTO accounts (id, email) VALUES (5, 'c@x.com')",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err:?}").to_ascii_lowercase();
    assert!(msg.contains("unique"), "expected UNIQUE error, got {msg}");
}

/// ON CONFLICT targeting a TEXT unique column resolves the existing row
/// through the indexed lookup and updates it in place instead of
/// duplicating -- the config-upsert shape downstream apps rely on.
#[test]
fn on_conflict_do_update_resolves_text_unique_target() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE kv (id INTEGER PRIMARY KEY, key TEXT UNIQUE, val TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO kv (id, key, val) VALUES (1, 'config', 'v1')",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO kv (id, key, val) VALUES (2, 'config', 'v2') \
         ON CONFLICT (key) DO UPDATE SET val = EXCLUDED.val",
        &[],
    )
    .unwrap();
    let rows = eng.sql("SELECT key, val FROM kv", &[]).unwrap();
    assert_eq!(rows.rows.len(), 1, "upsert must not duplicate the row");
    assert_eq!(
        rows.rows[0].get("val").cloned(),
        Some(uqa_core::Value::Str("v2".into()))
    );
}

/// Temporal keys are outside the value index's semantics guard; the
/// probe must fall back to the evaluated scan and still reject the
/// duplicate.
#[test]
fn unique_temporal_column_still_rejects_duplicates_via_scan_fallback() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE events (id INTEGER PRIMARY KEY, at TIMESTAMP UNIQUE)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO events (id, at) VALUES (1, '2026-01-01 00:00:00')",
        &[],
    )
    .unwrap();
    let err = eng
        .sql(
            "INSERT INTO events (id, at) VALUES (2, '2026-01-01 00:00:00')",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err:?}").to_ascii_lowercase();
    assert!(msg.contains("unique"), "expected UNIQUE error, got {msg}");
}

/// Composite FOREIGN KEY validation narrows through the one indexed
/// reference column and must verify the remaining column on the
/// candidates: a child row whose first key half matches an existing
/// parent but whose second half does not must still be rejected.
#[test]
fn composite_foreign_key_verifies_non_pivot_columns() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE parents (id INTEGER PRIMARY KEY, code TEXT UNIQUE, region INTEGER, \
         UNIQUE (code, region))",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE children (id INTEGER PRIMARY KEY, code TEXT, region INTEGER, \
         FOREIGN KEY (code, region) REFERENCES parents (code, region))",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO parents (id, code, region) VALUES (1, 'kr', 82)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO children (id, code, region) VALUES (1, 'kr', 82)",
        &[],
    )
    .unwrap();
    let err = eng
        .sql(
            "INSERT INTO children (id, code, region) VALUES (2, 'kr', 81)",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err:?}").to_ascii_lowercase();
    assert!(
        msg.contains("foreign key"),
        "expected FOREIGN KEY error, got {msg}"
    );
}
