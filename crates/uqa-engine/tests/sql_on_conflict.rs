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
fn on_conflict_do_update_reads_excluded_qualified_columns() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE engine_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO engine_meta (key, value) VALUES ('schema_version', '14')",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO engine_meta (key, value) VALUES ('schema_version', '15') \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        &[],
    )
    .unwrap();

    let row = eng.get_document("engine_meta", 1).unwrap();
    assert_eq!(row.get("value"), Some(&Value::Str("15".into())));
}

#[test]
fn on_conflict_do_update_reads_excluded_bound_params() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE engine_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO engine_meta (key, value) VALUES ('schema_version', '14')",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO engine_meta (key, value) VALUES ('schema_version', $1) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        &[uqa_sql::SQLParam::scalar(Value::Str("15".into()))],
    )
    .unwrap();

    let row = eng.get_document("engine_meta", 1).unwrap();
    assert_eq!(row.get("value"), Some(&Value::Str("15".into())));
}

#[test]
fn on_conflict_do_update_reads_excluded_bound_params_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("engine_meta.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE engine_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO engine_meta (key, value) VALUES ('schema_version', $1) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            &[uqa_sql::SQLParam::scalar(Value::Str("14".into()))],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO engine_meta (key, value) VALUES ('schema_version', $1) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            &[uqa_sql::SQLParam::scalar(Value::Str("15".into()))],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO engine_meta (key, value) VALUES ('local_seq', $1) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            &[uqa_sql::SQLParam::scalar(Value::Str("42".into()))],
        )
        .unwrap();
    }

    let eng = Engine::open(&db).unwrap();
    let version = eng
        .sql(
            "SELECT value FROM engine_meta WHERE key = 'schema_version'",
            &[],
        )
        .unwrap();
    assert_eq!(version.rows[0]["value"], Value::Str("15".into()));

    let seq = eng
        .sql("SELECT value FROM engine_meta WHERE key = 'local_seq'", &[])
        .unwrap();
    assert_eq!(seq.rows[0]["value"], Value::Str("42".into()));
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
