//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent sessions must preserve every successfully inserted row.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use uqa_core::Value;
use uqa_engine::{Engine, SQLFunctionOptions, SQLFunctionVolatility};

#[path = "concurrent_inserts/processes.rs"]
mod processes;

fn open_backend(path: &Path, backend: &str) -> Engine {
    match backend {
        "sqlite" => Engine::open(path).unwrap(),
        "compressed" => Engine::open_compressed(
            path,
            uqa_storage_sqlite::SQLiteCompressionOptions::default(),
        )
        .unwrap(),
        "redb" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

fn read_rows(reader: &Engine) -> Vec<BTreeMap<String, Value>> {
    reader
        .sql("SELECT key, _doc_id AS doc_id FROM items ORDER BY key", &[])
        .unwrap()
        .rows
}

fn concurrent_rows(first: Engine, second: Engine, command: &str) -> Vec<BTreeMap<String, Value>> {
    concurrent_row_batches(first, second, command, 1)
}

fn concurrent_row_batches(
    first: Engine,
    second: Engine,
    command: &str,
    rows_per_writer: usize,
) -> Vec<BTreeMap<String, Value>> {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    for session in [&first, &second] {
        let entered_tx = entered_tx.clone();
        let release_rx = Arc::clone(&release_rx);
        session
            .register_scalar_function_with_options(
                "insert_checkpoint",
                SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                move |_args: &[Value]| {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10))
                        .unwrap();
                    Ok(Value::Int(1))
                },
            )
            .unwrap();
    }
    let writers = [(first, "first"), (second, "second")]
        .into_iter()
        .map(|(session, key)| {
            let command = command.to_string();
            thread::spawn(move || {
                let result = session
                    .sql(
                        &command,
                        &[uqa_sql::SQLParam::Scalar(Value::Str(key.into()))],
                    )
                    .unwrap();
                assert_eq!(result.affected_rows, rows_per_writer as u64);
                assert_eq!(result.rows.len(), rows_per_writer);
                if session.transaction_depth() != 0 {
                    session.sql("COMMIT", &[]).unwrap();
                }
                result
                    .rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, mut row)| {
                        let expected_key = if rows_per_writer == 1 {
                            key.to_string()
                        } else {
                            format!("{key}{}", index + 1)
                        };
                        assert_eq!(row["key"], Value::Str(expected_key));
                        assert_eq!(row.remove("checkpoint"), Some(Value::Int(1)));
                        row
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    // RETURNING runs after physical identity allocation, while both statements still hold the snapshot from before either row was committed.
    for _ in 0..2 {
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    for _ in 0..2 * rows_per_writer {
        release.send(()).unwrap();
    }
    let rows = writers
        .into_iter()
        .flat_map(|writer| writer.join().unwrap())
        .collect::<Vec<_>>();
    let ids = rows
        .iter()
        .map(|row| {
            let Value::Int(id) = row["doc_id"] else {
                panic!("expected physical document identity")
            };
            id
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), rows.len());
    rows
}

#[test]
fn text_primary_key_inserts_survive_concurrent_preparation_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("concurrent-text-primary-keys.db");
    let root = Engine::open(&path).unwrap();
    root.sql("CREATE TABLE items (key TEXT PRIMARY KEY)", &[])
        .unwrap();
    let first = root.new_session().unwrap();
    let second = root.new_session().unwrap();
    let expected = concurrent_rows(first, second, "INSERT INTO items VALUES ($1) RETURNING key, _doc_id AS doc_id, insert_checkpoint() AS checkpoint");
    assert_eq!(read_rows(&root.new_session().unwrap()), expected);
    drop(root);
    assert_eq!(read_rows(&Engine::open(&path).unwrap()), expected);
}

#[test]
fn synthetic_identities_are_reserved_across_providers_and_insert_sources() {
    for backend in ["sqlite", "compressed", "redb"] {
        for definition in [
            "key TEXT",
            "key TEXT PRIMARY KEY",
            "key TEXT, part INTEGER DEFAULT 0, PRIMARY KEY (key, part)",
        ] {
            for source in ["VALUES ($1)", "SELECT $1"] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("synthetic-identities.db");
                let root = open_backend(&path, backend);
                root.sql(&format!("CREATE TABLE items ({definition})"), &[])
                    .unwrap();
                let first = root.new_session().unwrap();
                let second = root.new_session().unwrap();
                let command = format!("INSERT INTO items (key) {source} RETURNING key, _doc_id AS doc_id, insert_checkpoint() AS checkpoint");
                let expected = concurrent_rows(first, second, &command);
                assert_eq!(read_rows(&root.new_session().unwrap()), expected);
                drop(root);
                assert_eq!(read_rows(&open_backend(&path, backend)), expected);
            }
        }
    }
}

#[test]
fn independently_opened_engines_reserve_distinct_document_identities() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("independent-inserts.db");
    let root = Engine::open(&path).unwrap();
    root.sql("CREATE TABLE items (key TEXT PRIMARY KEY)", &[])
        .unwrap();
    let expected = concurrent_rows(
        Engine::open(&path).unwrap(),
        Engine::open(&path).unwrap(),
        "INSERT INTO items VALUES ($1) RETURNING key, _doc_id AS doc_id, insert_checkpoint() AS checkpoint",
    );
    assert_eq!(read_rows(&root), expected);
    drop(root);
    assert_eq!(read_rows(&Engine::open(&path).unwrap()), expected);
}

#[test]
fn multi_row_identity_reservations_survive_snapshot_refresh_and_writer_promotion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("multi-row-identities.db");
    let root = Engine::open(&path).unwrap();
    root.sql("CREATE TABLE items (key TEXT PRIMARY KEY)", &[])
        .unwrap();
    let first = root.new_session().unwrap();
    let second = root.new_session().unwrap();
    for session in [&first, &second] {
        session.sql("BEGIN; SAVEPOINT before_insert", &[]).unwrap();
    }
    let expected = concurrent_row_batches(first, second,
        "INSERT INTO items VALUES ($1 || '1'), ($1 || '2') RETURNING key, _doc_id AS doc_id, insert_checkpoint() AS checkpoint", 2);
    assert_eq!(read_rows(&root.new_session().unwrap()), expected);
    drop(root);
    assert_eq!(read_rows(&Engine::open(&path).unwrap()), expected);
}

#[test]
fn identity_reservation_rechecks_commits_after_the_statement_snapshot() {
    for backend in ["sqlite", "redb"] {
        for isolation in [None, Some("REPEATABLE READ"), Some("SERIALIZABLE")] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("committed-identity.db");
            let root = open_backend(&path, backend);
            root.sql(
                "CREATE TABLE items (key TEXT PRIMARY KEY, payload INTEGER)",
                &[],
            )
            .unwrap();
            let (entered, release) = super::register_blocking_scalar(&root, "before_identity");
            let session = root.new_session().unwrap();
            if let Some(isolation) = isolation {
                session
                    .sql(&format!("BEGIN ISOLATION LEVEL {isolation}"), &[])
                    .unwrap();
                assert!(read_rows(&session).is_empty());
            }
            let writer = thread::spawn(move || {
                let result = session.sql("INSERT INTO items VALUES ('second', before_identity()) RETURNING key, _doc_id AS doc_id", &[]).unwrap();
                if isolation.is_some() {
                    session.sql("COMMIT", &[]).unwrap();
                }
                result.rows
            });
            entered.recv_timeout(Duration::from_secs(10)).unwrap();
            let mut expected = root
                .sql(
                    "INSERT INTO items VALUES ('first', 1) RETURNING key, _doc_id AS doc_id",
                    &[],
                )
                .unwrap()
                .rows;
            release.send(()).unwrap();
            expected.extend(writer.join().unwrap());
            assert_ne!(expected[0]["doc_id"], expected[1]["doc_id"]);
            assert_eq!(read_rows(&root.new_session().unwrap()), expected);
            drop(root);
            assert_eq!(read_rows(&open_backend(&path, backend)), expected);
        }
    }
}

#[test]
fn failed_and_rolled_back_inserts_release_identity_reservations() {
    for backend in ["sqlite", "compressed", "redb"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rolled-back-identities.db");
        let root = open_backend(&path, backend);
        root.sql("CREATE TABLE items (key TEXT PRIMARY KEY)", &[])
            .unwrap();
        root.register_scalar_function_with_options(
            "fail_after_identity",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            |_args: &[Value]| {
                Err(uqa_sql::SQLError::Internal(
                    "injected RETURNING failure".into(),
                ))
            },
        )
        .unwrap();
        let session = root.new_session().unwrap();
        let error = session
            .sql(
                "INSERT INTO items VALUES ('failed') RETURNING fail_after_identity()",
                &[],
            )
            .unwrap_err();
        assert!(error.to_string().contains("injected RETURNING failure"));
        assert!(read_rows(&root).is_empty());
        let rolled_back = session.sql("BEGIN; SAVEPOINT before_insert; INSERT INTO items VALUES ('rolled_back') RETURNING _doc_id AS doc_id", &[]).unwrap();
        assert_eq!(rolled_back.rows[0]["doc_id"], Value::Int(1));
        session
            .sql("ROLLBACK TO before_insert; COMMIT", &[])
            .unwrap();
        let replacement = root
            .new_session()
            .unwrap()
            .sql(
                "INSERT INTO items VALUES ('kept') RETURNING key, _doc_id AS doc_id",
                &[],
            )
            .unwrap();
        assert_eq!(replacement.rows[0]["doc_id"], Value::Int(1));
        assert_eq!(read_rows(&root), replacement.rows);
        drop(session);
        drop(root);
        assert_eq!(read_rows(&open_backend(&path, backend)), replacement.rows);
    }
}
