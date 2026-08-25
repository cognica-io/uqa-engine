//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Referential actions for PostgreSQL-style FOREIGN KEY constraints.

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn query(engine: &Engine, sql: &str) -> uqa_sql::SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn assert_err_contains(engine: &Engine, sql: &str, needle: &str) {
    let err = engine.sql(sql, &[]).unwrap_err();
    assert!(
        format!("{err:?}").contains(needle),
        "error {err:?} did not contain {needle:?}"
    );
}

fn assert_sqlstate(engine: &Engine, sql: &str, expected: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(expected), "{sql}: {error}");
}

#[test]
fn on_delete_cascade_removes_child_rows() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parent(id) ON DELETE CASCADE
        )",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1), (2)");
    exec(
        &engine,
        "INSERT INTO child (id, parent_id) VALUES (10, 1), (11, 2)",
    );

    exec(&engine, "DELETE FROM parent WHERE id = 1");

    let rows = query(&engine, "SELECT id, parent_id FROM child ORDER BY id");
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0]["id"], Value::Int(11));
    assert_eq!(rows.rows[0]["parent_id"], Value::Int(2));
}

#[test]
fn on_update_cascade_rewrites_child_key() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parent(id) ON UPDATE CASCADE
        )",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1)");
    exec(&engine, "INSERT INTO child (id, parent_id) VALUES (10, 1)");

    exec(&engine, "UPDATE parent SET id = 3 WHERE id = 1");

    let rows = query(&engine, "SELECT parent_id FROM child WHERE id = 10");
    assert_eq!(rows.rows[0]["parent_id"], Value::Int(3));
}

#[test]
fn on_delete_set_null_rewrites_child_key() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parent(id) ON DELETE SET NULL
        )",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1)");
    exec(&engine, "INSERT INTO child (id, parent_id) VALUES (10, 1)");

    exec(&engine, "DELETE FROM parent WHERE id = 1");

    let rows = query(&engine, "SELECT parent_id FROM child WHERE id = 10");
    assert_eq!(rows.rows[0]["parent_id"], Value::Null);
}

#[test]
fn on_delete_set_default_rewrites_child_key_and_revalidates() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER DEFAULT 2 REFERENCES parent(id) ON DELETE SET DEFAULT
        )",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1), (2)");
    exec(&engine, "INSERT INTO child (id, parent_id) VALUES (10, 1)");

    exec(&engine, "DELETE FROM parent WHERE id = 1");

    let rows = query(&engine, "SELECT parent_id FROM child WHERE id = 10");
    assert_eq!(rows.rows[0]["parent_id"], Value::Int(2));
}

#[test]
fn on_update_set_default_rewrites_child_key_and_revalidates() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER DEFAULT 2 REFERENCES parent(id) ON UPDATE SET DEFAULT
        )",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1), (2)");
    exec(&engine, "INSERT INTO child (id, parent_id) VALUES (10, 1)");

    exec(&engine, "UPDATE parent SET id = 3 WHERE id = 1");

    let rows = query(&engine, "SELECT parent_id FROM child WHERE id = 10");
    assert_eq!(rows.rows[0]["parent_id"], Value::Int(2));
}

#[test]
fn on_update_set_null_rewrites_child_key() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parent(id) ON UPDATE SET NULL
        )",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1)");
    exec(&engine, "INSERT INTO child (id, parent_id) VALUES (10, 1)");

    exec(&engine, "UPDATE parent SET id = 3 WHERE id = 1");

    let rows = query(&engine, "SELECT parent_id FROM child WHERE id = 10");
    assert_eq!(rows.rows[0]["parent_id"], Value::Null);
}

#[test]
fn on_delete_set_null_column_list_only_rewrites_named_columns() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE users (tenant_id INTEGER, id INTEGER, name TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE posts (
            id INTEGER PRIMARY KEY,
            tenant_id INTEGER,
            author_id INTEGER DEFAULT NULL,
            FOREIGN KEY (tenant_id, author_id)
                REFERENCES users(tenant_id, id)
                ON DELETE SET NULL (author_id)
        )",
    );
    exec(
        &engine,
        "INSERT INTO users (tenant_id, id, name) VALUES (7, 42, 'ann')",
    );
    exec(
        &engine,
        "INSERT INTO posts (id, tenant_id, author_id) VALUES (1, 7, 42)",
    );

    exec(&engine, "DELETE FROM users WHERE tenant_id = 7 AND id = 42");

    let rows = query(
        &engine,
        "SELECT tenant_id, author_id FROM posts WHERE id = 1",
    );
    assert_eq!(rows.rows[0]["tenant_id"], Value::Int(7));
    assert_eq!(rows.rows[0]["author_id"], Value::Null);
}

#[test]
fn on_delete_set_default_column_list_only_rewrites_named_columns() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE users (tenant_id INTEGER, id INTEGER, name TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE posts (
            id INTEGER PRIMARY KEY,
            tenant_id INTEGER,
            author_id INTEGER DEFAULT 0,
            FOREIGN KEY (tenant_id, author_id)
                REFERENCES users(tenant_id, id)
                ON DELETE SET DEFAULT (author_id)
        )",
    );
    exec(
        &engine,
        "INSERT INTO users (tenant_id, id, name) VALUES (7, 0, 'fallback'), (7, 42, 'ann')",
    );
    exec(
        &engine,
        "INSERT INTO posts (id, tenant_id, author_id) VALUES (1, 7, 42)",
    );

    exec(&engine, "DELETE FROM users WHERE tenant_id = 7 AND id = 42");

    let rows = query(
        &engine,
        "SELECT tenant_id, author_id FROM posts WHERE id = 1",
    );
    assert_eq!(rows.rows[0]["tenant_id"], Value::Int(7));
    assert_eq!(rows.rows[0]["author_id"], Value::Int(0));
}

#[test]
fn self_referential_on_delete_cascade_removes_descendants() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE node (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES node(id) ON DELETE CASCADE
        )",
    );
    exec(
        &engine,
        "INSERT INTO node (id, parent_id) VALUES (1, NULL), (2, 1), (3, 2)",
    );

    exec(&engine, "DELETE FROM node WHERE id = 1");

    assert!(query(&engine, "SELECT id FROM node").rows.is_empty());
}

#[test]
fn set_null_failure_rolls_back_parent_delete() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER NOT NULL REFERENCES parent(id) ON DELETE SET NULL
        )",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1)");
    exec(&engine, "INSERT INTO child (id, parent_id) VALUES (10, 1)");

    assert_err_contains(
        &engine,
        "DELETE FROM parent WHERE id = 1",
        "NOT NULL constraint",
    );

    assert_eq!(query(&engine, "SELECT id FROM parent").rows.len(), 1);
    let rows = query(&engine, "SELECT parent_id FROM child WHERE id = 10");
    assert_eq!(rows.rows[0]["parent_id"], Value::Int(1));
}

#[test]
fn match_full_rejects_partially_null_composite_key() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (a INTEGER, b INTEGER)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            a INTEGER,
            b INTEGER,
            FOREIGN KEY (a, b) REFERENCES parent(a, b) MATCH FULL
        )",
    );
    exec(&engine, "INSERT INTO parent (a, b) VALUES (1, 2)");

    assert_err_contains(
        &engine,
        "INSERT INTO child (id, a, b) VALUES (10, NULL, 2)",
        "MATCH FULL",
    );
    exec(
        &engine,
        "INSERT INTO child (id, a, b) VALUES (11, NULL, NULL)",
    );
    exec(&engine, "INSERT INTO child (id, a, b) VALUES (12, 1, 2)");

    let rows = query(&engine, "SELECT id FROM child ORDER BY id");
    assert_eq!(rows.rows.len(), 2);
}

#[test]
fn insert_select_rejects_missing_foreign_key_and_rolls_back() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parent(id)
        )",
    );
    exec(
        &engine,
        "CREATE TABLE source (id INTEGER PRIMARY KEY, parent_id INTEGER)",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1)");
    exec(
        &engine,
        "INSERT INTO source (id, parent_id) VALUES (10, 1), (11, 99)",
    );

    assert_sqlstate(
        &engine,
        "INSERT INTO child (id, parent_id) SELECT id, parent_id FROM source",
        "23503",
    );

    assert!(query(&engine, "SELECT id FROM child").rows.is_empty());
}

#[test]
fn on_conflict_update_cascades_referenced_key() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parent(id) ON UPDATE CASCADE
        )",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1)");
    exec(&engine, "INSERT INTO child (id, parent_id) VALUES (10, 1)");

    exec(
        &engine,
        "INSERT INTO parent (id) VALUES (1)
         ON CONFLICT (id) DO UPDATE SET id = 2",
    );

    let rows = query(&engine, "SELECT parent_id FROM child WHERE id = 10");
    assert_eq!(rows.rows[0]["parent_id"], Value::Int(2));
}

#[test]
fn merge_update_cascades_referenced_key() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parent(id) ON UPDATE CASCADE
        )",
    );
    exec(
        &engine,
        "CREATE TABLE delta (old_id INTEGER, new_id INTEGER)",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1)");
    exec(&engine, "INSERT INTO child (id, parent_id) VALUES (10, 1)");
    exec(&engine, "INSERT INTO delta (old_id, new_id) VALUES (1, 3)");

    exec(
        &engine,
        "MERGE INTO parent AS p USING delta AS d ON p.id = d.old_id
         WHEN MATCHED THEN UPDATE SET id = d.new_id",
    );

    let rows = query(&engine, "SELECT parent_id FROM child WHERE id = 10");
    assert_eq!(rows.rows[0]["parent_id"], Value::Int(3));
}

#[test]
fn merge_delete_runs_on_delete_cascade() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parent(id) ON DELETE CASCADE
        )",
    );
    exec(&engine, "CREATE TABLE delta (id INTEGER)");
    exec(&engine, "INSERT INTO parent (id) VALUES (1)");
    exec(&engine, "INSERT INTO child (id, parent_id) VALUES (10, 1)");
    exec(&engine, "INSERT INTO delta (id) VALUES (1)");

    exec(
        &engine,
        "MERGE INTO parent AS p USING delta AS d ON p.id = d.id
         WHEN MATCHED THEN DELETE",
    );

    assert!(query(&engine, "SELECT id FROM child").rows.is_empty());
}

#[test]
fn merge_insert_rejects_missing_foreign_key_and_rolls_back() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    exec(
        &engine,
        "CREATE TABLE child (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parent(id)
        )",
    );
    exec(
        &engine,
        "CREATE TABLE source (id INTEGER PRIMARY KEY, parent_id INTEGER)",
    );
    exec(&engine, "INSERT INTO parent (id) VALUES (1)");
    exec(
        &engine,
        "INSERT INTO source (id, parent_id) VALUES (10, 1), (11, 99)",
    );

    assert_sqlstate(
        &engine,
        "MERGE INTO child AS c USING source AS s ON c.id = s.id
         WHEN NOT MATCHED THEN INSERT (id, parent_id) VALUES (s.id, s.parent_id)",
        "23503",
    );

    assert!(query(&engine, "SELECT id FROM child").rows.is_empty());
}
