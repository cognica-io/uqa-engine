//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::mpsc;
use std::time::Duration;

use super::*;

fn integer_column(result: &SQLResult, name: &str) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match row.get(name) {
            Some(crate::Value::Int(value)) => *value,
            other => panic!("expected integer column {name}, got {other:?}"),
        })
        .collect()
}

fn end_backend_transaction_early(engine: &Engine) {
    engine
        .storage
        .backend
        .as_ref()
        .expect("persistent test engine")
        .rollback_transaction()
        .expect("end backend transaction early");
}

fn assert_combined_panic_and_rollback_error(error: &str) {
    assert!(error.contains("rollback"), "{error}");
    assert!(error.contains("original panic: callback panic"), "{error}");
}

#[test]
fn dropped_transaction_scope_rolls_back_data_and_releases_writer_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("scope-cleanup.db")).unwrap();
    engine
        .sql(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)",
            &[],
        )
        .unwrap();
    engine.sql("INSERT INTO items VALUES (1, 10)", &[]).unwrap();
    let sibling = engine.new_session().unwrap();

    {
        let _scope = TransactionScope::begin(&engine).unwrap();
        engine
            .sql("UPDATE items SET value = 20 WHERE id = 1", &[])
            .unwrap();
        assert_eq!(engine.transaction_depth(), 1);
    }

    assert_eq!(engine.transaction_depth(), 0);
    sibling
        .sql("UPDATE items SET value = value + 1 WHERE id = 1", &[])
        .unwrap();
    assert_eq!(
        integer_column(
            &engine
                .sql("SELECT value FROM items WHERE id = 1", &[])
                .unwrap(),
            "value",
        ),
        [11]
    );
}

#[test]
fn unclosed_nested_callback_frames_are_rejected_and_rolled_back() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("unbalanced-scope.db")).unwrap();
    engine
        .sql(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)",
            &[],
        )
        .unwrap();
    engine.sql("INSERT INTO items VALUES (1, 10)", &[]).unwrap();

    let error = engine
        .transaction(|engine| {
            engine
                .sql("UPDATE items SET value = 20 WHERE id = 1", &[])
                .unwrap();
            engine.begin()?;
            engine
                .sql("UPDATE items SET value = 30 WHERE id = 1", &[])
                .unwrap();
            Ok(())
        })
        .unwrap_err();

    assert!(error.to_string().contains("changed scoped frame depth"));
    assert_eq!(engine.transaction_depth(), 0);
    assert_eq!(
        integer_column(
            &engine
                .sql("SELECT value FROM items WHERE id = 1", &[])
                .unwrap(),
            "value",
        ),
        [10]
    );
}

#[test]
fn implicit_read_transaction_rolls_back_an_unclassified_storage_write() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("read-only-guard.db")).unwrap();

    engine.begin_implicit_statement_transaction(true).unwrap();
    engine
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .set_metadata("hidden-write", "x")
        .unwrap();

    let error = engine.commit().unwrap_err().to_string();
    assert!(error.contains("read-only SQL execution"), "{error}");
    assert_eq!(engine.transaction_depth(), 0);
    assert_eq!(
        engine
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .get_metadata("hidden-write")
            .unwrap(),
        None
    );
}

#[test]
fn rollback_failure_after_callback_panic_is_returned_instead_of_panicking_again() {
    let directory = tempfile::tempdir().unwrap();

    let transaction_engine = Engine::open(&directory.path().join("transaction.db")).unwrap();
    let transaction_result: Result<(), SQLError> = transaction_engine.transaction(|engine| {
        end_backend_transaction_early(engine);
        panic!("callback panic");
    });
    let transaction_error = transaction_result.unwrap_err();
    assert_combined_panic_and_rollback_error(&transaction_error.to_string());
    assert_eq!(transaction_engine.transaction_depth(), 0);

    let storage_engine = Engine::open(&directory.path().join("storage.db")).unwrap();
    let storage_result: StorageBackendResult<()> = storage_engine
        .with_implicit_storage_transaction(|engine| {
            end_backend_transaction_early(engine);
            panic!("callback panic");
        });
    let storage_error = storage_result.unwrap_err();
    assert_combined_panic_and_rollback_error(&storage_error.to_string());
    assert_eq!(storage_engine.transaction_depth(), 0);

    let string_engine = Engine::open(&directory.path().join("string.db")).unwrap();
    let string_result: Result<(), String> =
        string_engine.with_implicit_string_transaction(|engine| {
            end_backend_transaction_early(engine);
            panic!("callback panic");
        });
    let string_error = string_result.unwrap_err();
    assert_combined_panic_and_rollback_error(&string_error);
    assert_eq!(string_engine.transaction_depth(), 0);
}

#[test]
fn waiting_writer_refreshes_when_sqlite_commit_precedes_epoch_publication() {
    for create_sql in [
        "CREATE TABLE fresh.items (id INTEGER)",
        "CREATE TABLE fresh.items AS SELECT 1 AS id",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = Engine::open(&directory.path().join("catalog-race.db")).unwrap();
        let writer = root.new_session().unwrap();
        let waiter = root.new_session().unwrap();

        writer.begin().unwrap();
        assert!(!waiter.has_schema("fresh").unwrap());
        writer.sql("CREATE SCHEMA fresh", &[]).unwrap();

        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let waiting_thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = waiter.sql(create_sql, &[]);
            done_tx.send(result).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        match done_rx.recv_timeout(Duration::from_millis(200)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("waiting writer result channel failed early: {error}"),
            Ok(result) => panic!("waiting writer completed before writer release: {result:?}"),
        }

        // End the physical transaction without publishing the shared epoch. This deterministically models the interval after SQLite COMMIT has released its writer lock but before Engine::commit publishes it. The logical writer registration goes with it, exactly as the real commit path releases the session's locks before publication.
        writer
            .storage
            .backend
            .as_ref()
            .unwrap()
            .commit_transaction()
            .unwrap();
        writer.row_locks.release_session(writer.session_id);

        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        waiting_thread.join().unwrap();
        writer.session.transactions.lock().clear();
        assert!(root
            .new_session()
            .unwrap()
            .has_table("fresh.items")
            .unwrap());
    }
}

#[test]
fn unchanged_persistent_statements_keep_their_loaded_catalog_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("stable-snapshot.db")).unwrap();
    engine
        .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
        .unwrap();

    // Consume the catalog/data generations published by CREATE TABLE.
    engine.sql("SELECT id FROM items", &[]).unwrap();
    let before = engine.require_table("items").unwrap();

    engine.sql("SELECT id FROM items", &[]).unwrap();
    let after = engine.require_table("items").unwrap();

    assert!(
        std::sync::Arc::ptr_eq(&before, &after),
        "an unchanged statement rebuilt the complete persistent catalog"
    );
}

#[test]
fn compressed_write_refresh_uses_the_pinned_transaction_connection() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open_compressed(
        &directory.path().join("compressed-write.db"),
        uqa_storage::SQLiteCompressionOptions::default(),
    )
    .unwrap();

    engine
        .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO items (id) VALUES (1)", &[])
        .unwrap();
    let result = engine.sql("SELECT id FROM items", &[]).unwrap();
    assert_eq!(result.rows.len(), 1);
}

#[test]
fn compressed_fixed_snapshot_releases_reader_locks_and_preserves_repeatable_read() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open_compressed(
        &directory.path().join("compressed-fixed-snapshot.db"),
        uqa_storage::SQLiteCompressionOptions::default(),
    )
    .unwrap();
    root.sql(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO items VALUES (1, 10), (2, 20)", &[])
        .unwrap();
    let reader = root.new_session().unwrap();
    let writer = root.new_session().unwrap();

    reader
        .sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_eq!(
        integer_column(
            &reader
                .sql("SELECT value FROM items ORDER BY id", &[])
                .unwrap(),
            "value",
        ),
        [10, 20]
    );
    writer
        .sql("UPDATE items SET value = 11 WHERE id = 1", &[])
        .unwrap();
    assert_eq!(
        integer_column(
            &writer
                .sql("SELECT value FROM items ORDER BY id", &[])
                .unwrap(),
            "value",
        ),
        [11, 20]
    );
    reader
        .sql("UPDATE items SET value = 21 WHERE id = 2", &[])
        .unwrap();
    assert_eq!(
        integer_column(
            &reader
                .sql("SELECT value FROM items ORDER BY id", &[])
                .unwrap(),
            "value",
        ),
        [10, 21]
    );
    reader.sql("COMMIT", &[]).unwrap();
    let observer = root.new_session().unwrap();
    assert_eq!(
        integer_column(
            &observer
                .sql("SELECT value FROM items ORDER BY id", &[])
                .unwrap(),
            "value",
        ),
        [11, 21]
    );
}

#[test]
fn pinned_reader_defers_sibling_catalog_epochs_until_transaction_end() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("pinned-reader.db")).unwrap();
    let reader = root.new_session().unwrap();
    let writer = root.new_session().unwrap();

    {
        let characteristics = reader.default_transaction_characteristics();
        let mut stack = reader.session.transactions.lock();
        reader
            .begin_transaction_frame(&mut stack, true, true, false, characteristics)
            .unwrap();
    }
    assert!(!reader.has_schema("later").unwrap());
    writer.sql("CREATE SCHEMA later", &[]).unwrap();
    writer.create_graph("later_graph").unwrap();

    assert!(!reader.has_schema("later").unwrap());
    assert!(!reader.has_graph("later_graph").unwrap());
    reader.commit().unwrap();

    assert!(reader.has_schema("later").unwrap());
    assert!(reader.has_graph("later_graph").unwrap());
}
