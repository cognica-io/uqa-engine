//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{Arc, Barrier};

use rusqlite::Connection;

use super::schema::{initialize_registry, open_registry};
use super::*;

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
