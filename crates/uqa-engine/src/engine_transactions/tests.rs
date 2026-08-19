//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::mpsc;
use std::time::Duration;

use super::*;

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
        let result = waiter.sql("CREATE TABLE fresh.items (id INTEGER)", &[]);
        done_tx.send(result).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());

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
fn pinned_reader_defers_sibling_catalog_epochs_until_transaction_end() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("pinned-reader.db")).unwrap();
    let reader = root.new_session().unwrap();
    let writer = root.new_session().unwrap();

    {
        let mut stack = reader.session.transactions.lock();
        reader
            .begin_transaction_frame(&mut stack, true, true)
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
