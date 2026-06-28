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
fn on_conflict_do_update_reads_target_table_qualified_columns() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE usage_daily_rollups (
            day TEXT NOT NULL,
            feature TEXT NOT NULL,
            action TEXT NOT NULL,
            event_count BIGINT NOT NULL DEFAULT 0,
            success_count BIGINT NOT NULL DEFAULT 0,
            failure_count BIGINT NOT NULL DEFAULT 0,
            cancelled_count BIGINT NOT NULL DEFAULT 0,
            total_duration_ms BIGINT NOT NULL DEFAULT 0,
            total_active_ms BIGINT NOT NULL DEFAULT 0,
            total_size_bytes BIGINT NOT NULL DEFAULT 0,
            updated_at BIGINT NOT NULL,
            PRIMARY KEY (day, feature, action)
        )",
        &[],
    )
    .unwrap();

    let upsert = "INSERT INTO usage_daily_rollups
        (day, feature, action, event_count, success_count, failure_count,
         cancelled_count, total_duration_ms, total_active_ms,
         total_size_bytes, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        ON CONFLICT (day, feature, action) DO UPDATE SET
          event_count = usage_daily_rollups.event_count + EXCLUDED.event_count,
          success_count = usage_daily_rollups.success_count + EXCLUDED.success_count,
          failure_count = usage_daily_rollups.failure_count + EXCLUDED.failure_count,
          cancelled_count = usage_daily_rollups.cancelled_count + EXCLUDED.cancelled_count,
          total_duration_ms = usage_daily_rollups.total_duration_ms + EXCLUDED.total_duration_ms,
          total_active_ms = usage_daily_rollups.total_active_ms + EXCLUDED.total_active_ms,
          total_size_bytes = usage_daily_rollups.total_size_bytes + EXCLUDED.total_size_bytes,
          updated_at = EXCLUDED.updated_at";
    let params = [
        uqa_sql::SQLParam::scalar(Value::Str("2026-06-28".into())),
        uqa_sql::SQLParam::scalar(Value::Str("chat".into())),
        uqa_sql::SQLParam::scalar(Value::Str("send".into())),
        uqa_sql::SQLParam::scalar(Value::Int(1)),
        uqa_sql::SQLParam::scalar(Value::Int(1)),
        uqa_sql::SQLParam::scalar(Value::Int(0)),
        uqa_sql::SQLParam::scalar(Value::Int(0)),
        uqa_sql::SQLParam::scalar(Value::Int(123)),
        uqa_sql::SQLParam::scalar(Value::Int(100)),
        uqa_sql::SQLParam::scalar(Value::Int(42)),
        uqa_sql::SQLParam::scalar(Value::Int(1_782_615_734_609)),
    ];

    eng.sql(upsert, &params).unwrap();
    eng.sql(upsert, &params).unwrap();

    let res = eng
        .sql(
            "SELECT event_count, success_count, total_duration_ms
             FROM usage_daily_rollups
             WHERE day = $1 AND feature = $2 AND action = $3",
            &params[..3],
        )
        .unwrap();
    assert_eq!(res.rows[0].get("event_count"), Some(&Value::Int(2)));
    assert_eq!(res.rows[0].get("success_count"), Some(&Value::Int(2)));
    assert_eq!(res.rows[0].get("total_duration_ms"), Some(&Value::Int(246)));
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
