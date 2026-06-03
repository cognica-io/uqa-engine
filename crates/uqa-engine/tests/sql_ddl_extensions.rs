//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the DDL surface added to match the UQA
//! engine: `BIGSERIAL` / `SERIAL` columns with auto-id INSERTs,
//! `DROP TABLE / INDEX [IF EXISTS]`, and `ALTER TABLE` action variants
//! (ADD / DROP / RENAME COLUMN, RENAME TO).

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn bigserial_auto_id_assigns_monotonic_ids() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE messages (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO messages (body) VALUES ('first')", &[])
        .unwrap();
    eng.sql("INSERT INTO messages (body) VALUES ('second')", &[])
        .unwrap();
    let res = eng
        .sql("SELECT id, body FROM messages ORDER BY id", &[])
        .unwrap();
    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0]["id"], Value::Int(1));
    assert_eq!(res.rows[0]["body"], Value::Str("first".into()));
    assert_eq!(res.rows[1]["id"], Value::Int(2));
    assert_eq!(res.rows[1]["body"], Value::Str("second".into()));
}

#[test]
fn serial_auto_id_with_explicit_id_advances_watermark() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id SERIAL PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id, body) VALUES (10, 'jump')", &[])
        .unwrap();
    eng.sql("INSERT INTO t (body) VALUES ('after-jump')", &[])
        .unwrap();
    let res = eng.sql("SELECT id FROM t ORDER BY id", &[]).unwrap();
    let ids: Vec<i64> = res
        .rows
        .iter()
        .map(|r| match r.get("id").expect("id column") {
            Value::Int(v) => *v,
            other => panic!("expected int, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![10, 11]);
}

#[test]
fn drop_table_if_exists_is_noop_when_missing() {
    let eng = Engine::new();
    eng.sql("DROP TABLE IF EXISTS does_not_exist", &[]).unwrap();
}

#[test]
fn drop_table_without_if_exists_errors_when_missing() {
    let eng = Engine::new();
    let err = eng.sql("DROP TABLE missing", &[]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("DROP TABLE"), "unexpected error: {msg}");
}

#[test]
fn drop_index_if_exists_is_noop() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("DROP INDEX IF EXISTS notes_body_idx", &[]).unwrap();
}

#[test]
fn drop_index_without_if_exists_errors_when_missing() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    let err = eng.sql("DROP INDEX notes_body_idx", &[]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("DROP INDEX"), "unexpected error: {msg}");
}

#[test]
fn alter_table_add_column_and_insert_uses_it() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("ALTER TABLE notes ADD COLUMN tag TEXT", &[])
        .unwrap();
    eng.sql("INSERT INTO notes (body, tag) VALUES ('hi', 'greet')", &[])
        .unwrap();
    let res = eng
        .sql("SELECT id, body, tag FROM notes ORDER BY id", &[])
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0]["tag"], Value::Str("greet".into()));
}

#[test]
fn alter_table_drop_column_removes_visibility() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT, tag TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (body, tag) VALUES ('payload', 'first')",
        &[],
    )
    .unwrap();
    eng.sql("ALTER TABLE notes DROP COLUMN tag", &[]).unwrap();
    assert!(!eng.table_has_column("notes", "tag"));
}

#[test]
fn alter_table_rename_column_propagates() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("ALTER TABLE notes RENAME COLUMN body TO content", &[])
        .unwrap();
    assert!(eng.table_has_column("notes", "content"));
    assert!(!eng.table_has_column("notes", "body"));
}

#[test]
fn bigserial_watermark_survives_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("uqa.db");
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            "CREATE TABLE messages (id BIGSERIAL PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
        eng.sql("INSERT INTO messages (body) VALUES ('a')", &[])
            .unwrap();
        eng.sql("INSERT INTO messages (body) VALUES ('b')", &[])
            .unwrap();
    }
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql("INSERT INTO messages (body) VALUES ('c')", &[])
            .unwrap();
        let res = eng.sql("SELECT id FROM messages ORDER BY id", &[]).unwrap();
        let ids: Vec<i64> = res
            .rows
            .iter()
            .map(|r| match r.get("id").expect("id") {
                Value::Int(v) => *v,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }
}

#[test]
fn alter_table_rename_table_moves_state() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO notes (body) VALUES ('hello')", &[])
        .unwrap();
    eng.sql("ALTER TABLE notes RENAME TO posts", &[]).unwrap();
    assert!(eng.has_table("posts"));
    assert!(!eng.has_table("notes"));
    let res = eng.sql("SELECT body FROM posts ORDER BY id", &[]).unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0]["body"], Value::Str("hello".into()));
}
