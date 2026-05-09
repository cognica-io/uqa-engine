//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of `uqa/tests/test_ddl.py`.

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
    let msg = err.to_string();
    assert!(
        msg.contains(needle),
        "expected error containing `{needle}`, got `{msg}`"
    );
}

fn engine_with_users() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)",
    );
    exec(
        &engine,
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30)",
    );
    exec(
        &engine,
        "INSERT INTO users (id, name, age) VALUES (2, 'Bob', 25)",
    );
    exec(
        &engine,
        "INSERT INTO users (id, name, age) VALUES (3, 'Carol', 35)",
    );
    engine
}

fn engine_with_parents() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE parents (id INT PRIMARY KEY, name TEXT)",
    );
    exec(&engine, "INSERT INTO parents VALUES (1, 'Parent1')");
    exec(&engine, "INSERT INTO parents VALUES (2, 'Parent2')");
    engine
}

fn create_children(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE children \
         (id INT PRIMARY KEY, parent_id INT REFERENCES parents(id), val TEXT)",
    );
}

#[test]
fn alter_table_add_column() {
    let engine = engine_with_users();
    exec(&engine, "ALTER TABLE users ADD COLUMN email TEXT");
    exec(
        &engine,
        "UPDATE users SET email = 'alice@test.com' WHERE id = 1",
    );
    let result = query(&engine, "SELECT email FROM users WHERE id = 1");
    assert_eq!(result.rows[0]["email"], Value::Str("alice@test.com".into()));
}

#[test]
fn alter_table_add_column_duplicate_raises() {
    let engine = engine_with_users();
    assert_err_contains(
        &engine,
        "ALTER TABLE users ADD COLUMN name TEXT",
        "already exists",
    );
}

#[test]
fn alter_table_add_column_with_default() {
    let engine = engine_with_users();
    exec(
        &engine,
        "ALTER TABLE users ADD COLUMN active BOOLEAN DEFAULT TRUE",
    );
    exec(
        &engine,
        "INSERT INTO users (id, name, age) VALUES (4, 'Dave', 28)",
    );
    let result = query(&engine, "SELECT active FROM users WHERE id = 4");
    assert_eq!(result.rows[0]["active"], Value::Bool(true));
}

#[test]
fn create_index_using_ivf_accepts_vector_columns() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(3))",
    );
    exec(
        &engine,
        "CREATE INDEX docs_embedding_ivf ON docs USING ivf (embedding) \
         WITH (lists = 4, probes = 2, train_threshold = 4)",
    );
}

#[test]
fn create_index_using_hnsw_aliases_ivf() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(3))",
    );
    exec(
        &engine,
        "CREATE INDEX docs_embedding_hnsw ON docs USING hnsw (embedding) \
         WITH (lists = 4, probes = 2, train_threshold = 4)",
    );
}

#[test]
fn alter_table_add_not_null_column_with_default_backfills_existing_rows() {
    let engine = engine_with_users();
    exec(
        &engine,
        "ALTER TABLE users ADD COLUMN retrieval_top_k INTEGER NOT NULL DEFAULT 0",
    );
    exec(&engine, "UPDATE users SET age = age + 1 WHERE id = 1");

    let result = query(&engine, "SELECT retrieval_top_k FROM users WHERE id = 1");
    assert_eq!(result.rows[0]["retrieval_top_k"], Value::Int(0));
}

#[test]
fn not_null_default_survives_engine_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("default.db");
    {
        let engine = Engine::open(&db).unwrap();
        exec(
            &engine,
            "CREATE TABLE conversations (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                retrieval_top_k INTEGER NOT NULL DEFAULT 0
            )",
        );
    }
    let engine = Engine::open(&db).unwrap();
    let columns = engine.describe_table("conversations").unwrap();
    let retrieval_col = columns
        .iter()
        .find(|col| col.name == "retrieval_top_k")
        .unwrap();
    assert!(retrieval_col.default.is_some());

    exec(
        &engine,
        "INSERT INTO conversations (id, title) VALUES ('c1', 'hello')",
    );
    let result = query(
        &engine,
        "SELECT retrieval_top_k FROM conversations WHERE id = 'c1'",
    );
    assert_eq!(result.rows[0]["retrieval_top_k"], Value::Int(0));
}

#[test]
fn alter_table_add_not_null_defaults_backfill_persistent_rows_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("conversations.db");
    {
        let engine = Engine::open(&db).unwrap();
        exec(
            &engine,
            "CREATE TABLE conversations (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                enabled_tools TEXT NOT NULL DEFAULT '[]'
            )",
        );
        exec(
            &engine,
            "INSERT INTO conversations (id, title) VALUES ('c1', 'hello')",
        );
    }

    {
        let engine = Engine::open(&db).unwrap();
        exec(
            &engine,
            "ALTER TABLE conversations ADD COLUMN retrieval_top_k INTEGER NOT NULL DEFAULT 0",
        );
        exec(
            &engine,
            "ALTER TABLE conversations ADD COLUMN retrieval_max_context_tokens INTEGER NOT NULL DEFAULT 0",
        );
        exec(
            &engine,
            "UPDATE conversations SET enabled_tools = '[\"web_search\"]' WHERE id = 'c1'",
        );

        let result = query(
            &engine,
            "SELECT retrieval_top_k, retrieval_max_context_tokens
             FROM conversations WHERE id = 'c1'",
        );
        assert_eq!(result.rows[0]["retrieval_top_k"], Value::Int(0));
        assert_eq!(
            result.rows[0]["retrieval_max_context_tokens"],
            Value::Int(0)
        );
    }

    let engine = Engine::open(&db).unwrap();
    exec(
        &engine,
        "UPDATE conversations SET title = 'still ok' WHERE id = 'c1'",
    );
    let result = query(
        &engine,
        "SELECT retrieval_top_k, retrieval_max_context_tokens
         FROM conversations WHERE id = 'c1'",
    );
    assert_eq!(result.rows[0]["retrieval_top_k"], Value::Int(0));
    assert_eq!(
        result.rows[0]["retrieval_max_context_tokens"],
        Value::Int(0)
    );
}

#[test]
fn alter_table_drop_column() {
    let engine = engine_with_users();
    exec(&engine, "ALTER TABLE users DROP COLUMN age");
    let result = query(&engine, "SELECT name FROM users WHERE id = 1");
    assert_eq!(result.rows[0]["name"], Value::Str("Alice".into()));
    let result = query(&engine, "SELECT * FROM users WHERE id = 1");
    assert!(!result.rows[0].contains_key("age"));
}

#[test]
fn alter_table_drop_column_nonexistent_raises() {
    let engine = engine_with_users();
    assert_err_contains(
        &engine,
        "ALTER TABLE users DROP COLUMN nonexistent",
        "does not exist",
    );
}

#[test]
fn alter_table_drop_column_if_exists() {
    let engine = engine_with_users();
    exec(
        &engine,
        "ALTER TABLE users DROP COLUMN IF EXISTS nonexistent",
    );
}

#[test]
fn alter_table_rename_column() {
    let engine = engine_with_users();
    exec(&engine, "ALTER TABLE users RENAME COLUMN name TO full_name");
    let result = query(&engine, "SELECT full_name FROM users WHERE id = 1");
    assert_eq!(result.rows[0]["full_name"], Value::Str("Alice".into()));
}

#[test]
fn alter_table_rename_column_nonexistent_raises() {
    let engine = engine_with_users();
    assert_err_contains(
        &engine,
        "ALTER TABLE users RENAME COLUMN xyz TO abc",
        "does not exist",
    );
}

#[test]
fn alter_table_rename_column_duplicate_raises() {
    let engine = engine_with_users();
    assert_err_contains(
        &engine,
        "ALTER TABLE users RENAME COLUMN name TO age",
        "already exists",
    );
}

#[test]
fn alter_table_rename_table() {
    let engine = engine_with_users();
    exec(&engine, "ALTER TABLE users RENAME TO people");
    let result = query(&engine, "SELECT COUNT(*) AS cnt FROM people");
    assert_eq!(result.rows[0]["cnt"], Value::Int(3));
    assert_err_contains(&engine, "SELECT * FROM users", "does not exist");
}

#[test]
fn alter_table_rename_table_duplicate_raises() {
    let engine = engine_with_users();
    exec(&engine, "CREATE TABLE other (id INTEGER)");
    assert_err_contains(
        &engine,
        "ALTER TABLE users RENAME TO other",
        "already exists",
    );
}

#[test]
fn alter_table_set_default() {
    let engine = engine_with_users();
    exec(&engine, "ALTER TABLE users ALTER COLUMN age SET DEFAULT 18");
    exec(&engine, "INSERT INTO users (id, name) VALUES (4, 'Dave')");
    let result = query(&engine, "SELECT age FROM users WHERE id = 4");
    assert_eq!(result.rows[0]["age"], Value::Int(18));
}

#[test]
fn alter_table_drop_default() {
    let engine = engine_with_users();
    exec(&engine, "ALTER TABLE users ALTER COLUMN age SET DEFAULT 18");
    exec(&engine, "ALTER TABLE users ALTER COLUMN age DROP DEFAULT");
    exec(&engine, "INSERT INTO users (id, name) VALUES (5, 'Eve')");
    let result = query(&engine, "SELECT age FROM users WHERE id = 5");
    assert!(matches!(
        result.rows[0].get("age"),
        None | Some(Value::Null)
    ));
}

#[test]
fn alter_table_set_not_null() {
    let engine = engine_with_users();
    exec(&engine, "ALTER TABLE users ALTER COLUMN name SET NOT NULL");
    assert_err_contains(
        &engine,
        "INSERT INTO users (id, age) VALUES (4, 28)",
        "NOT NULL",
    );
}

#[test]
fn alter_table_set_not_null_with_existing_nulls_raises() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER, val TEXT)");
    exec(&engine, "INSERT INTO t (id) VALUES (1)");
    assert_err_contains(
        &engine,
        "ALTER TABLE t ALTER COLUMN val SET NOT NULL",
        "contains NULL",
    );
}

#[test]
fn alter_table_drop_not_null() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER, val TEXT NOT NULL)");
    exec(&engine, "ALTER TABLE t ALTER COLUMN val DROP NOT NULL");
    exec(&engine, "INSERT INTO t (id) VALUES (1)");
    let result = query(&engine, "SELECT val FROM t WHERE id = 1");
    assert!(matches!(
        result.rows[0].get("val"),
        None | Some(Value::Null)
    ));
}

#[test]
fn truncate_table_basic() {
    let engine = engine_with_users();
    exec(&engine, "TRUNCATE TABLE users");
    let result = query(&engine, "SELECT COUNT(*) AS cnt FROM users");
    assert_eq!(result.rows[0]["cnt"], Value::Int(0));
}

#[test]
fn truncate_table_resets_auto_increment() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id SERIAL PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO t (val) VALUES ('a')");
    exec(&engine, "INSERT INTO t (val) VALUES ('b')");
    exec(&engine, "TRUNCATE TABLE t");
    exec(&engine, "INSERT INTO t (val) VALUES ('c')");
    let result = query(&engine, "SELECT id FROM t");
    assert_eq!(result.rows[0]["id"], Value::Int(1));
}

#[test]
fn truncate_table_preserves_schema() {
    let engine = engine_with_users();
    exec(&engine, "TRUNCATE TABLE users");
    exec(
        &engine,
        "INSERT INTO users (id, name, age) VALUES (1, 'New', 20)",
    );
    let result = query(&engine, "SELECT name FROM users WHERE id = 1");
    assert_eq!(result.rows[0]["name"], Value::Str("New".into()));
}

#[test]
fn truncate_table_nonexistent_raises() {
    let engine = Engine::new();
    assert_err_contains(&engine, "TRUNCATE TABLE nonexistent", "does not exist");
}

#[test]
fn unique_constraint_basic() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER, email TEXT UNIQUE)");
    exec(
        &engine,
        "INSERT INTO t (id, email) VALUES (1, 'a@test.com')",
    );
    assert_err_contains(
        &engine,
        "INSERT INTO t (id, email) VALUES (2, 'a@test.com')",
        "UNIQUE constraint",
    );
}

#[test]
fn unique_constraint_allows_different_values() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER, email TEXT UNIQUE)");
    exec(
        &engine,
        "INSERT INTO t (id, email) VALUES (1, 'a@test.com')",
    );
    exec(
        &engine,
        "INSERT INTO t (id, email) VALUES (2, 'b@test.com')",
    );
    let result = query(&engine, "SELECT COUNT(*) AS cnt FROM t");
    assert_eq!(result.rows[0]["cnt"], Value::Int(2));
}

#[test]
fn unique_constraint_allows_null() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER, email TEXT UNIQUE)");
    exec(
        &engine,
        "INSERT INTO t (id, email) VALUES (1, 'a@test.com')",
    );
    exec(&engine, "INSERT INTO t (id) VALUES (2)");
    exec(&engine, "INSERT INTO t (id) VALUES (3)");
    let result = query(&engine, "SELECT COUNT(*) AS cnt FROM t");
    assert_eq!(result.rows[0]["cnt"], Value::Int(3));
}

#[test]
fn primary_key_enforces_uniqueness() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO t (id, val) VALUES (1, 'a')");
    assert_err_contains(
        &engine,
        "INSERT INTO t (id, val) VALUES (1, 'b')",
        "UNIQUE constraint",
    );
}

#[test]
fn check_constraint_basic() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER, age INTEGER CHECK (age > 0))",
    );
    exec(&engine, "INSERT INTO t (id, age) VALUES (1, 25)");
    assert_err_contains(
        &engine,
        "INSERT INTO t (id, age) VALUES (2, -1)",
        "CHECK constraint",
    );
}

#[test]
fn check_constraint_allows_valid() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER, age INTEGER CHECK (age > 0))",
    );
    exec(&engine, "INSERT INTO t (id, age) VALUES (1, 1)");
    exec(&engine, "INSERT INTO t (id, age) VALUES (2, 100)");
    let result = query(&engine, "SELECT COUNT(*) AS cnt FROM t");
    assert_eq!(result.rows[0]["cnt"], Value::Int(2));
}

#[test]
fn check_constraint_with_comparison() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER, price REAL CHECK (price >= 0.0))",
    );
    exec(&engine, "INSERT INTO t (id, price) VALUES (1, 9.99)");
    assert_err_contains(
        &engine,
        "INSERT INTO t (id, price) VALUES (2, -0.01)",
        "CHECK constraint",
    );
}

#[test]
fn alter_column_type_change_type() {
    let engine = engine_with_users();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER, name TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (1, 10, 'alpha')",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (2, 20, 'bravo')",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (3, 30, 'charlie')",
    );
    exec(&engine, "ALTER TABLE t ALTER COLUMN val TYPE TEXT");
    let result = query(&engine, "SELECT val FROM t WHERE id = 1");
    assert_eq!(result.rows[0]["val"], Value::Str("10".into()));
}

#[test]
fn foreign_key_basic_insert() {
    let engine = engine_with_parents();
    create_children(&engine);
    exec(&engine, "INSERT INTO children VALUES (1, 1, 'child1')");
    let result = query(&engine, "SELECT id, parent_id, val FROM children");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["parent_id"], Value::Int(1));
}

#[test]
fn foreign_key_insert_violation() {
    let engine = engine_with_parents();
    create_children(&engine);
    assert_err_contains(
        &engine,
        "INSERT INTO children VALUES (1, 999, 'bad')",
        "FOREIGN KEY constraint violated",
    );
}

#[test]
fn foreign_key_null_allowed() {
    let engine = engine_with_parents();
    create_children(&engine);
    exec(&engine, "INSERT INTO children VALUES (1, NULL, 'orphan')");
    let result = query(&engine, "SELECT id, parent_id, val FROM children");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["parent_id"], Value::Null);
}

#[test]
fn foreign_key_delete_violation() {
    let engine = engine_with_parents();
    create_children(&engine);
    exec(&engine, "INSERT INTO children VALUES (1, 1, 'child1')");
    assert_err_contains(
        &engine,
        "DELETE FROM parents WHERE id = 1",
        "FOREIGN KEY constraint violated",
    );
}

#[test]
fn foreign_key_delete_unreferenced() {
    let engine = engine_with_parents();
    create_children(&engine);
    exec(&engine, "INSERT INTO children VALUES (1, 1, 'child1')");
    exec(&engine, "DELETE FROM parents WHERE id = 2");
    let result = query(&engine, "SELECT id, name FROM parents");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["id"], Value::Int(1));
}

#[test]
fn foreign_key_update_violation() {
    let engine = engine_with_parents();
    create_children(&engine);
    exec(&engine, "INSERT INTO children VALUES (1, 1, 'child1')");
    assert_err_contains(
        &engine,
        "UPDATE children SET parent_id = 999 WHERE id = 1",
        "FOREIGN KEY constraint violated",
    );
}

#[test]
fn foreign_key_update_valid() {
    let engine = engine_with_parents();
    create_children(&engine);
    exec(&engine, "INSERT INTO children VALUES (1, 1, 'child1')");
    exec(&engine, "UPDATE children SET parent_id = 2 WHERE id = 1");
    let result = query(&engine, "SELECT parent_id FROM children WHERE id = 1");
    assert_eq!(result.rows[0]["parent_id"], Value::Int(2));
}

#[test]
fn foreign_key_update_parent_pk_violation() {
    let engine = engine_with_parents();
    create_children(&engine);
    exec(&engine, "INSERT INTO children VALUES (1, 1, 'child1')");
    assert_err_contains(
        &engine,
        "UPDATE parents SET id = 99 WHERE id = 1",
        "FOREIGN KEY constraint violated",
    );
}
