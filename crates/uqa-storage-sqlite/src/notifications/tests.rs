//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{Arc, Barrier};

use rusqlite::Connection;

use super::schema::{initialize_registry, open_registry};
use super::*;
mod publication;
mod serialized;

#[test]
fn registry_initialization_serializes_concurrent_first_open() {
    for key in [
        None,
        Some(StorageEncryptionKey::new("concurrent-registry-test-key")),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = Arc::new(directory.path().join("notification-state"));
        let barrier = Arc::new(Barrier::new(8));
        let workers = (0..8)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                let key = key.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    open_registry(&path, key.as_ref()).map(|_| ())
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        let registry = open_registry(&path, key.as_ref()).unwrap();
        let connection = registry.lease_connection().unwrap();
        let mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
    }
}

#[test]
fn registry_open_rejects_missing_versioned_state_instead_of_repairing_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notification-state");
    let registry = open_registry(&path, None).unwrap();
    Connection::open(registry.database_path().unwrap())
        .unwrap()
        .execute_batch("DROP TABLE listeners")
        .unwrap();
    let error = initialize_registry(&registry).unwrap_err();
    assert!(
        error.contains("validate asynchronous notification registry listeners"),
        "{error}"
    );
}

#[test]
fn dropped_registry_transaction_discards_prepared_queue_changes() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("database");
    let registry = open_registry(&database, None).unwrap();

    {
        let transaction = open_registry_transaction(&registry).unwrap();
        transaction
            .append_entries(&[NotificationQueueEntry {
                sequence: 0,
                process_id: 1,
                channel: "events".into(),
                payload: "prepared".into(),
            }])
            .unwrap();
        transaction
            .save_queue_state(NotificationQueueState {
                next_sequence: 1,
                head_position: 20,
            })
            .unwrap();
    }
    let transaction = open_registry_transaction(&registry).unwrap();
    assert_eq!(
        transaction.queue_state().unwrap(),
        NotificationQueueState::default()
    );
    assert!(transaction.entries_from(0).unwrap().is_empty());
    transaction.commit().unwrap();
}

fn legacy_registry(database: &std::path::Path) -> Connection {
    let mut path = database.as_os_str().to_owned();
    path.push(".uqa-notification-state");
    let connection = Connection::open(std::path::PathBuf::from(path)).unwrap();
    connection.execute_batch(
        "PRAGMA application_id = 1431391793;
         PRAGMA user_version = 1;
         CREATE TABLE queue_state (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), next_sequence INTEGER NOT NULL CHECK (next_sequence >= 0), head_position INTEGER NOT NULL CHECK (head_position >= 0)) STRICT;
         INSERT INTO queue_state VALUES (1, 7, 140);
         CREATE TABLE backend_process_id_state (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), next_process_id INTEGER NOT NULL CHECK (next_process_id BETWEEN 1 AND 2147483648)) STRICT;
         INSERT INTO backend_process_id_state VALUES (1, 43);
         CREATE TABLE queue_entries (sequence INTEGER PRIMARY KEY CHECK (sequence >= 0), process_id INTEGER NOT NULL CHECK (process_id > 0), channel TEXT NOT NULL, payload TEXT NOT NULL) STRICT;
         INSERT INTO queue_entries VALUES (6, 42, 'events', 'preserved');
         CREATE TABLE listeners (owner_id BLOB NOT NULL CHECK (length(owner_id) = 16), session_id BLOB NOT NULL CHECK (length(session_id) = 8), process_id INTEGER NOT NULL CHECK (process_id > 0), wake_port INTEGER NOT NULL CHECK (wake_port BETWEEN 1 AND 65535), channels_json TEXT NOT NULL, transaction_open INTEGER NOT NULL CHECK (transaction_open IN (0, 1)), next_sequence INTEGER NOT NULL CHECK (next_sequence >= 0), position INTEGER NOT NULL CHECK (position >= 0), PRIMARY KEY (owner_id, session_id)) STRICT;
         INSERT INTO listeners VALUES (x'01010101010101010101010101010101', x'0000000000000001', 42, 1234, '[\"events\"]', 1, 6, 120);"
    ).unwrap();
    connection
}

#[test]
fn registry_upgrade_preserves_live_state_and_fences_already_open_cached_writers() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("upgraded");
    let legacy = legacy_registry(&database);
    let update = "UPDATE queue_state SET next_sequence = next_sequence + 1";
    drop(legacy.prepare_cached(update).unwrap());
    let registry = NotificationRegistry::open(&database, None).unwrap();
    for statement in [
        update,
        "UPDATE backend_process_id_state SET next_process_id = next_process_id + 1",
        "UPDATE queue_entries SET payload = 'incompatible'",
        "UPDATE listeners SET position = position + 1",
        "UPDATE publication_state SET next_publication = next_publication + 1",
    ] {
        let error = legacy
            .prepare_cached(statement)
            .and_then(|mut statement| statement.execute([]))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("__uqa_notification_writer_format"),
            "{error}"
        );
    }
    let transaction = registry.begin().unwrap();
    assert_eq!(
        transaction.queue_state().unwrap(),
        NotificationQueueState {
            next_sequence: 7,
            head_position: 140
        }
    );
    assert_eq!(transaction.entries_from(0).unwrap()[0].payload, "preserved");
    let listener = transaction.listeners().unwrap().pop().unwrap();
    assert_eq!(listener.position, 120);
    assert_eq!(listener.next_sequence, 6);
    assert!(listener.transaction_open);
    assert_eq!(transaction.allocate_backend_process_id().unwrap(), 43);
    transaction.commit().unwrap();
    assert_eq!(
        legacy
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn a_retained_registry_handle_revalidates_format_and_releases_failed_admission() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("changed-format");
    let registry = NotificationRegistry::open(&database, None).unwrap();
    let external = Connection::open(registry.connection.database_path().unwrap()).unwrap();
    external.pragma_update(None, "user_version", 3).unwrap();
    assert!(registry.begin().is_err());
    external.pragma_update(None, "user_version", 2).unwrap();
    registry.begin().unwrap().commit().unwrap();
    external
        .execute_batch("DROP TRIGGER notification_writer_listeners_UPDATE")
        .unwrap();
    assert!(NotificationRegistry::open(&database, None).is_err());
    let remains_missing: i64 = external.query_row("SELECT count(*) FROM sqlite_schema WHERE name = 'notification_writer_listeners_UPDATE'", [], |row| row.get(0)).unwrap();
    assert_eq!(remains_missing, 0);
}
