//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recovery across the actual durable-data and notification-publication boundary.

use super::*;
use std::path::Path;
use uqa_core::Value;

mod process;

#[test]
fn cancelled_serialized_notification_commit_clears_its_transaction_frame() {
    let directory = tempfile::tempdir().unwrap();
    let sender = open(3, &directory.path().join("cancelled-notification.db"));
    sender
        .sql("BEGIN READ ONLY; NOTIFY cancelled_event", &[])
        .unwrap();
    sender.runtime.cancellation.cancel();
    let error = sender.commit().unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"), "{error}");
    assert!(sender.session.transactions.lock().is_empty());
    sender.runtime.cancellation.reset();
    assert_eq!(sender.sql("SELECT 1", &[]).unwrap().rows.len(), 1);
}

const KEY: &str = "notification-recovery-fixture-key";

fn open(provider: usize, path: &Path) -> Engine {
    match provider {
        0 => Engine::open(path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => {
            let connection = uqa_storage_sqlite::ManagedConnection::open(path).unwrap();
            Engine::from_persistent_backends(
                Arc::new(uqa_storage_sqlite::Catalog::open(connection.clone()).unwrap()),
                Arc::new(uqa_storage_sqlite::SQLiteStorageBackend::new(connection)),
            )
            .unwrap()
        }
        4 => Engine::open_encrypted(path, KEY).unwrap(),
        5 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::from_connection(
                uqa_storage_sqlite::ManagedConnection::open_encrypted(path, KEY).unwrap(),
            )
            .unwrap(),
        ))
        .unwrap(),
        6 => {
            let connection =
                uqa_storage_sqlite::ManagedConnection::open_encrypted(path, KEY).unwrap();
            Engine::from_persistent_backends(
                Arc::new(uqa_storage_sqlite::Catalog::open(connection.clone()).unwrap()),
                Arc::new(uqa_storage_sqlite::SQLiteStorageBackend::new(connection)),
            )
            .unwrap()
        }
        7 => Engine::open_compressed_encrypted(
            path,
            KEY,
            uqa_storage_sqlite::SQLiteCompressionOptions::default(),
        )
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
fn committed_notifications_survive_sender_loss_around_queue_publication(
    #[values(0, 1, 2, 3)] provider: usize,
    #[values(false, true)] registry_committed: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let sender = open(provider, &directory.path().join("notifications.db"));
    sender.sql("CREATE TABLE items (id INTEGER)", &[]).unwrap();
    let listener = sender.new_session().unwrap();
    listener.sql("LISTEN committed_items", &[]).unwrap();
    let process_id = sender.backend_process_id();
    sender
        .sql(
            "BEGIN; INSERT INTO items VALUES (1); NOTIFY committed_items, 'one'",
            &[],
        )
        .unwrap();
    assert!(listener
        .sql("SELECT id FROM items", &[])
        .unwrap()
        .rows
        .is_empty());
    assert!(listener.take_sql_notifications().is_empty());

    let stack = sender.session.transactions.lock();
    let mut guard = sender
        .begin_notification_commit(true, stack.last().unwrap())
        .unwrap()
        .unwrap();
    sender
        .storage
        .backend
        .as_ref()
        .unwrap()
        .commit_transaction()
        .unwrap();
    if registry_committed {
        guard
            .cross
            .as_mut()
            .unwrap()
            .registry
            .take()
            .unwrap()
            .commit()
            .unwrap();
    }
    drop(guard);
    drop(stack);
    drop(sender);

    let committed = listener.sql("SELECT id FROM items", &[]).unwrap();
    assert_eq!(committed.value_at(0, 0), Some(&Value::Int(1)));
    listener.poll_sql_notifications().unwrap();
    let delivered = listener.take_sql_notifications();
    assert_eq!(delivered.len(), 1, "committed publication was lost");
    assert_eq!(delivered[0].process_id, process_id);
    assert_eq!(delivered[0].channel, "committed_items");
    assert_eq!(delivered[0].payload, "one");
    listener.poll_sql_notifications().unwrap();
    assert!(listener.take_sql_notifications().is_empty());
}

#[rstest::rstest]
fn registry_commit_failure_preserves_committed_data_and_listener_changes(
    #[values(0, 1, 2, 3)] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("completion.db");
    let sender = open(provider, &path);
    sender.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
    let listener = sender.new_session().unwrap();
    listener.sql("LISTEN committed_items", &[]).unwrap();
    let observer = sender.new_session().unwrap();
    let mut registry_path = path.as_os_str().to_owned();
    registry_path.push(".uqa-notification-state");
    let registry = rusqlite::Connection::open(Path::new(&registry_path)).unwrap();
    registry.execute_batch("CREATE TABLE required_publication(id INTEGER PRIMARY KEY); CREATE TABLE rejected_publication(id INTEGER REFERENCES required_publication(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_publication_commit AFTER INSERT ON queue_entries BEGIN INSERT INTO rejected_publication VALUES(1); END;").unwrap();
    let error = sender.sql("BEGIN; INSERT INTO items VALUES(1); LISTEN later_items; NOTIFY committed_items, 'original'; COMMIT", &[]).unwrap_err();
    assert!(
        error.to_string().contains("transaction committed"),
        "{error}"
    );
    assert_eq!(sender.transaction_depth(), 0);
    assert_eq!(sender.listening_channels(), ["later_items"]);
    assert_eq!(
        observer
            .sql("SELECT id FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(1))
    );
    registry
        .execute_batch("DROP TRIGGER reject_publication_commit")
        .unwrap();
    listener.poll_sql_notifications().unwrap();
    let delivered = listener.take_sql_notifications();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].process_id, sender.backend_process_id());
    assert_eq!(delivered[0].payload, "original");
    observer
        .sql("NOTIFY later_items, 'after committed listen'", &[])
        .unwrap();
    sender.poll_sql_notifications().unwrap();
    let delivered = sender.take_sql_notifications();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].payload, "after committed listen");
    listener.poll_sql_notifications().unwrap();
    assert!(listener.take_sql_notifications().is_empty());
}

#[rstest::rstest]
fn recovered_publication_preserves_listener_boundaries(
    #[values(0, 1, 2, 3)] provider: usize,
    #[values(false, true)] retain_original_listener: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let sender = open(provider, &directory.path().join("listeners.db"));
    sender.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
    let original = sender.new_session().unwrap();
    original.sql("LISTEN committed_items", &[]).unwrap();
    original
        .sql(
            "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT id FROM items",
            &[],
        )
        .unwrap();
    sender
        .sql(
            "BEGIN; INSERT INTO items VALUES(1); NOTIFY committed_items, 'historical'",
            &[],
        )
        .unwrap();
    let stack = sender.session.transactions.lock();
    let guard = sender
        .begin_notification_commit(true, stack.last().unwrap())
        .unwrap()
        .unwrap();
    sender
        .storage
        .backend
        .as_ref()
        .unwrap()
        .commit_transaction()
        .unwrap();
    drop(guard);
    drop(stack);
    let later = sender.new_session().unwrap();
    drop(sender);
    let original = retain_original_listener.then_some(original);
    later.sql("LISTEN committed_items", &[]).unwrap();
    later.poll_sql_notifications().unwrap();
    assert!(later.take_sql_notifications().is_empty());
    if let Some(original) = original {
        assert!(original
            .sql("SELECT id FROM items", &[])
            .unwrap()
            .rows
            .is_empty());
        assert!(original.take_sql_notifications().is_empty());
        original.sql("COMMIT", &[]).unwrap();
        let delivered = original.take_sql_notifications();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].payload, "historical");
        original.poll_sql_notifications().unwrap();
        assert!(original.take_sql_notifications().is_empty());
    }
    later.sql("NOTIFY committed_items, 'current'", &[]).unwrap();
    let delivered = later.take_sql_notifications();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].payload, "current");
}
