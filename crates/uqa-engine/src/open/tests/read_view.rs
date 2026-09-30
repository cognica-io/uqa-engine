//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact read-view identity avoids catalog scans while preserving writes and undo.

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage::PersistentStorageBackend;

use super::*;

#[test]
fn seeded_session_reuses_the_validated_catalog_on_its_first_pinned_read() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let root = Engine::from_persistent_backends(
        Arc::new(catalog),
        Arc::new(SQLiteStorageBackend::new(connection.clone())),
    )
    .unwrap();
    root.release_automatic_statistics_client();
    root.session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
    root.sql("CREATE TABLE items (id INTEGER)", &[]).unwrap();
    let session = root.new_session().unwrap();
    session.release_automatic_statistics_client();
    session
        .session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
    let backend = session.storage.backend.as_ref().unwrap();
    backend.begin_read_transaction().unwrap();
    connection
        .new_session()
        .with_physical(|sqlite| {
            sqlite.flush_prepared_statement_cache();
            sqlite.authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Read {
                    table_name: "_uqa_mvcc_heads" | "_uqa_mvcc_versions" | "_uqa_mvcc_runs",
                    ..
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))?;
            Ok(())
        })
        .unwrap();
    session.refresh_pinned_transaction_snapshot().unwrap();
    connection
        .new_session()
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            Ok(())
        })
        .unwrap();
    backend.rollback_transaction().unwrap();
}

#[test]
fn failed_cache_restoration_cannot_reuse_an_earlier_token_after_undo() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Arc::new(Catalog::open(connection.clone()).unwrap());
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let backend = Arc::new(SQLiteStorageBackend::new(connection.clone()));
    let engine = Engine::from_persistent_backends(catalog.clone(), backend.clone()).unwrap();
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
    backend.begin_transaction().unwrap();
    engine.refresh_pinned_transaction_snapshot().unwrap();
    connection.savepoint("unchanged").unwrap();
    catalog.set_metadata("read_view_probe", "private").unwrap();
    connection
        .new_session()
        .with_physical(|sqlite| {
            sqlite.flush_prepared_statement_cache();
            sqlite.authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Read {
                    table_name: "_uqa_mvcc_heads" | "_uqa_mvcc_versions" | "_uqa_mvcc_runs",
                    ..
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))?;
            Ok(())
        })
        .unwrap();
    assert!(engine.refresh_pinned_transaction_snapshot().is_err());
    connection.rollback_to_savepoint("unchanged").unwrap();
    assert!(
        engine.refresh_pinned_transaction_snapshot().is_err(),
        "undo must retry restoration after a failed refresh"
    );
    connection
        .new_session()
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            Ok(())
        })
        .unwrap();
    engine.refresh_pinned_transaction_snapshot().unwrap();
    backend.rollback_transaction().unwrap();
}

#[test]
fn private_catalog_cache_follows_ddl_undo_and_replacement_branches() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("private-catalog-read-view.db")).unwrap();
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
    // Independent reference: tests/parity/pg18/native_read_view_oracle.sql (PostgreSQL 18.4).
    engine
        .sql(
            "CREATE TABLE items (id INTEGER); INSERT INTO items VALUES (1)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "BEGIN; SAVEPOINT original; ALTER TABLE items ADD COLUMN label TEXT DEFAULT 'first'",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.sql("SELECT label FROM items", &[]).unwrap().rows[0]["label"],
        Value::Str("first".into())
    );
    engine.sql("ROLLBACK TO original", &[]).unwrap();
    assert!(!engine.sql("SELECT * FROM items", &[]).unwrap().rows[0].contains_key("label"));
    engine
        .sql(
            "ALTER TABLE items ADD COLUMN label TEXT DEFAULT 'replacement'",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.sql("SELECT label FROM items", &[]).unwrap().rows[0]["label"],
        Value::Str("replacement".into())
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert!(!engine.sql("SELECT * FROM items", &[]).unwrap().rows[0].contains_key("label"));
}

#[test]
fn repeated_pinned_refresh_uses_no_record_scan_even_after_a_private_write() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let engine = Engine::from_persistent_backends(
        Arc::new(catalog),
        Arc::new(SQLiteStorageBackend::new(connection.clone())),
    )
    .unwrap();
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
    engine.sql("CREATE TABLE items (id INTEGER)", &[]).unwrap();
    for private in [false, true] {
        engine.sql("BEGIN", &[]).unwrap();
        if private {
            engine.sql("INSERT INTO items VALUES (1)", &[]).unwrap();
        }
        engine.refresh_pinned_transaction_snapshot().unwrap();
        connection
            .new_session()
            .with_physical(|sqlite| {
                sqlite.flush_prepared_statement_cache();
                sqlite.authorizer(Some(|context: AuthContext<'_>| match context.action {
                    AuthAction::Read {
                        table_name: "_uqa_mvcc_heads" | "_uqa_mvcc_versions" | "_uqa_mvcc_runs",
                        ..
                    } => Authorization::Deny,
                    _ => Authorization::Allow,
                }))?;
                Ok(())
            })
            .unwrap();
        for _ in 0..4 {
            engine.refresh_pinned_transaction_snapshot().unwrap();
        }
        connection
            .new_session()
            .with_physical(|sqlite| {
                sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
                Ok(())
            })
            .unwrap();
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn cache_identity_tracks_savepoint_undo_and_other_session_commits() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("read-view.db")).unwrap();
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
    engine
        .sql(
            "CREATE TABLE items (id INTEGER); INSERT INTO items VALUES (1)",
            &[],
        )
        .unwrap();
    let other = engine.new_session().unwrap();
    other.release_automatic_statistics_client();
    other
        .session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
    engine.sql("BEGIN ISOLATION LEVEL REPEATABLE READ; SAVEPOINT empty; INSERT INTO items VALUES (2); SAVEPOINT keep; INSERT INTO items VALUES (3)", &[]).unwrap();
    let count = |engine: &Engine| {
        engine
            .sql("SELECT count(*) AS n FROM items", &[])
            .unwrap()
            .rows[0]["n"]
            .clone()
    };
    assert_eq!(count(&engine), Value::Int(3));
    other.sql("INSERT INTO items VALUES (4)", &[]).unwrap();
    assert_eq!(count(&engine), Value::Int(3));
    engine.sql("ROLLBACK TO keep", &[]).unwrap();
    assert_eq!(count(&engine), Value::Int(2));
    engine.sql("ROLLBACK TO empty", &[]).unwrap();
    assert_eq!(count(&engine), Value::Int(1));
    engine.sql("INSERT INTO items VALUES (5)", &[]).unwrap();
    assert_eq!(count(&engine), Value::Int(2));
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(count(&engine), Value::Int(2));
    let ids = engine
        .sql("SELECT id FROM items ORDER BY id", &[])
        .unwrap()
        .rows;
    assert_eq!(
        ids.iter().map(|row| row["id"].clone()).collect::<Vec<_>>(),
        [Value::Int(1), Value::Int(4)]
    );
}
