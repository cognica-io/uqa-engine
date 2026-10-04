//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A transaction or a single statement larger than its session's memory allowance spills its private changes to encrypted temporary files and completes, as `PostgreSQL` completes a transaction or a statement of any size.

use std::path::Path;
use std::sync::Arc;

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage_sqlite::{ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider};

/// A session allowance far smaller than the transactions below, so that they spill.
const ALLOWANCE: usize = 4 << 20;

fn engines(directory: &Path) -> Vec<(&'static str, Engine)> {
    engines_with(directory, ALLOWANCE)
}

fn engines_with(directory: &Path, allowance: usize) -> Vec<(&'static str, Engine)> {
    let options = VersionedSessionOptions {
        retained_bytes: allowance,
    };
    let native = {
        let connection = ManagedConnection::open(&directory.join("native.db")).unwrap();
        connection.bind_native_records(options).unwrap();
        Engine::from_persistent_provider(Arc::new(SQLiteStorageProvider::new(connection))).unwrap()
    };
    let key_value = Engine::from_persistent_provider(Arc::new(
        SQLiteKeyValueStorage::open_with_options(&directory.join("key-value.db"), options).unwrap(),
    ))
    .unwrap();
    let redb = Engine::from_persistent_provider(Arc::new(
        uqa_storage_redb::RedbStorage::open_with_options(directory.join("store.redb"), options)
            .unwrap(),
    ))
    .unwrap();
    vec![
        ("native SQLite", native),
        ("SQLite KeyValue", key_value),
        ("redb", redb),
    ]
}

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.rows[0].values().next().unwrap().clone()
}

/// Insert `rows` rows into `docs` in statements of `chunk` rows, starting after `first`.
fn insert_rows(engine: &Engine, first: i64, rows: i64, chunk: i64, body: &str) {
    let mut start = first + 1;
    while start <= first + rows {
        let end = (start + chunk - 1).min(first + rows);
        exec(
            engine,
            &format!(
                "INSERT INTO docs SELECT g, repeat('{body} ', 12) || g FROM generate_series({start}, {end}) AS g"
            ),
        );
        start = end + 1;
    }
}

#[test]
fn transactions_larger_than_the_session_allowance_commit() {
    let directory = tempfile::tempdir().unwrap();
    for (backend, engine) in engines(directory.path()) {
        exec(
            &engine,
            "CREATE TABLE docs (id integer PRIMARY KEY, body text NOT NULL)",
        );
        exec(&engine, "BEGIN");
        insert_rows(&engine, 0, 40_000, 2_000, "payload");
        exec(
            &engine,
            "UPDATE docs SET body = 'updated' WHERE id % 1000 = 0",
        );
        exec(&engine, "COMMIT");
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM docs"),
            Value::Int(40_000),
            "{backend}"
        );
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM docs WHERE body = 'updated'"),
            Value::Int(40),
            "{backend}"
        );
        assert_eq!(
            scalar(&engine, "SELECT body FROM docs WHERE id = 31234"),
            Value::Str(format!("{}31234", "payload ".repeat(12))),
            "{backend}"
        );
    }
}

#[test]
fn a_rollback_to_a_savepoint_discards_only_the_changes_after_it_across_spills() {
    let directory = tempfile::tempdir().unwrap();
    for (backend, engine) in engines(directory.path()) {
        exec(
            &engine,
            "CREATE TABLE docs (id integer PRIMARY KEY, body text NOT NULL)",
        );
        exec(&engine, "BEGIN");
        insert_rows(&engine, 0, 25_000, 2_500, "kept");
        exec(&engine, "SAVEPOINT later");
        insert_rows(&engine, 25_000, 25_000, 2_500, "discarded");
        exec(&engine, "UPDATE docs SET body = 'changed' WHERE id <= 100");
        exec(&engine, "ROLLBACK TO SAVEPOINT later");
        exec(&engine, "UPDATE docs SET body = 'renamed' WHERE id = 7");
        exec(&engine, "COMMIT");
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM docs"),
            Value::Int(25_000),
            "{backend}"
        );
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM docs WHERE body = 'changed'"),
            Value::Int(0),
            "{backend}"
        );
        assert_eq!(
            scalar(&engine, "SELECT body FROM docs WHERE id = 7"),
            Value::Str("renamed".into()),
            "{backend}"
        );
        assert_eq!(
            scalar(&engine, "SELECT max(id) FROM docs"),
            Value::Int(25_000),
            "{backend}"
        );
    }
}

/// The rows each statement below writes, more than `ALLOWANCE` holds.
const STATEMENT_ROWS: i64 = 12_000;

#[test]
fn statements_larger_than_the_session_allowance_complete() {
    let directory = tempfile::tempdir().unwrap();
    for (backend, engine) in engines(directory.path()) {
        exec(
            &engine,
            "CREATE TABLE docs (id integer PRIMARY KEY, body text NOT NULL)",
        );
        insert_rows(&engine, 0, STATEMENT_ROWS, STATEMENT_ROWS, "payload");
        exec(&engine, "UPDATE docs SET body = body || ' updated'");
        exec(&engine, "BEGIN");
        exec(&engine, "DELETE FROM docs WHERE id % 2 = 0");
        exec(&engine, "UPDATE docs SET body = 'renamed' WHERE id % 3 = 0");
        exec(&engine, "COMMIT");
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM docs"),
            Value::Int(STATEMENT_ROWS / 2),
            "{backend}"
        );
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM docs WHERE body = 'renamed'"),
            Value::Int(STATEMENT_ROWS / 6),
            "{backend}"
        );
        assert_eq!(
            scalar(&engine, "SELECT body FROM docs WHERE id = 7"),
            Value::Str(format!("{}7 updated", "payload ".repeat(12))),
            "{backend}"
        );
    }
}

#[test]
fn keys_stay_unique_across_the_rows_of_a_statement_larger_than_the_session_allowance() {
    let directory = tempfile::tempdir().unwrap();
    for (backend, engine) in engines(directory.path()) {
        exec(
            &engine,
            "CREATE TABLE docs (id integer PRIMARY KEY, body text NOT NULL)",
        );
        let duplicate = engine
            .sql(
                &format!(
                    "INSERT INTO docs SELECT g, repeat('payload ', 12) || g FROM generate_series(1, {STATEMENT_ROWS}) AS g UNION ALL SELECT 1, 'again'"
                ),
                &[],
            )
            .unwrap_err();
        assert_eq!(
            duplicate.sqlstate(),
            Some("23505"),
            "{backend}: {duplicate}"
        );
        assert!(
            duplicate
                .to_string()
                .contains("duplicate key value violates unique constraint \"docs_pkey\""),
            "{backend}: {duplicate}"
        );
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM docs"),
            Value::Int(0),
            "{backend}"
        );
        exec(
            &engine,
            &format!(
                "INSERT INTO docs SELECT g % {} + 1, repeat('payload ', 12) || g FROM generate_series(1, {STATEMENT_ROWS}) AS g ON CONFLICT (id) DO NOTHING",
                STATEMENT_ROWS / 2
            ),
        );
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM docs"),
            Value::Int(STATEMENT_ROWS / 2),
            "{backend}"
        );
        let twice = engine
            .sql(
                &format!(
                    "INSERT INTO docs SELECT g, repeat('payload ', 12) || g FROM generate_series({}, {}) AS g UNION ALL SELECT {}, 'again' ON CONFLICT (id) DO UPDATE SET body = excluded.body",
                    STATEMENT_ROWS / 2 + 1,
                    STATEMENT_ROWS * 3 / 2,
                    STATEMENT_ROWS / 2 + 1
                ),
                &[],
            )
            .unwrap_err();
        assert_eq!(twice.sqlstate(), Some("21000"), "{backend}: {twice}");
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM docs"),
            Value::Int(STATEMENT_ROWS / 2),
            "{backend}"
        );
    }
}

/// The rows of the statement below and an allowance they exceed. Each call of its function reads the table, so the statement's work grows with the square of its rows.
const READ_BACK_ROWS: i64 = 2_000;
const READ_BACK_ALLOWANCE: usize = 512 << 10;

#[test]
fn a_volatile_function_reads_the_rows_that_a_statement_larger_than_the_session_allowance_wrote_before(
) {
    let directory = tempfile::tempdir().unwrap();
    for (backend, engine) in engines_with(directory.path(), READ_BACK_ALLOWANCE) {
        exec(
            &engine,
            "CREATE TABLE docs (id integer PRIMARY KEY, body text NOT NULL)",
        );
        exec(
            &engine,
            "CREATE FUNCTION earlier(k integer) RETURNS text VOLATILE LANGUAGE sql AS $$ SELECT count(*)::text FROM docs WHERE id = k - 1 $$",
        );
        exec(
            &engine,
            &format!(
                "INSERT INTO docs SELECT g, earlier(g) || repeat(' payload', 12) FROM generate_series(1, {READ_BACK_ROWS}) AS g"
            ),
        );
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM docs WHERE body LIKE '1 %'"),
            Value::Int(READ_BACK_ROWS - 1),
            "{backend}"
        );
        assert_eq!(
            scalar(&engine, "SELECT body FROM docs WHERE id = 1"),
            Value::Str(format!("0{}", " payload".repeat(12))),
            "{backend}"
        );
    }
}
