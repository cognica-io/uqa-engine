//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real native lock and VM cancellation with original transaction and pool ownership.

use super::{
    control, registry_error, Connection, NotificationQueueEntry, NotificationQueueState,
    NotificationRegistry, StorageBackendError, StorageBackendResult,
};
use std::time::Duration;
use uqa_storage::read_control::StorageReadControl;

fn notify_waiter(
    control: &StorageReadControl,
    waiting: &std::sync::mpsc::Receiver<()>,
    receiver: &std::sync::mpsc::Receiver<StorageBackendResult<()>>,
    worker: std::thread::JoinHandle<()>,
) {
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    control.cancellation().cancel();
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
        Err(StorageBackendError::Cancelled(_))
    ));
    worker.join().unwrap();
}

#[test]
fn writer_admission_cancels_without_releasing_the_original_blocker() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("busy.db"), None).unwrap();
    let blocker = registry.begin().unwrap();
    let control = StorageReadControl::with_limit(4_096);
    let (send, receive) = std::sync::mpsc::channel();
    let (waiting, observed) = std::sync::mpsc::sync_channel(1);
    let worker = {
        let registry = registry.clone();
        let control = control.clone();
        std::thread::spawn(move || {
            control::observe_native_wait(waiting);
            send.send(registry.begin_with_control(&control).map(|_| ()))
                .unwrap();
        })
    };
    notify_waiter(&control, &observed, &receive, worker);
    assert_eq!(
        blocker.queue_state().unwrap(),
        NotificationQueueState::default()
    );
    blocker.commit().unwrap();
    registry.begin().unwrap().commit().unwrap();
}

#[test]
fn moved_transaction_commit_wait_cancels_and_rolls_back_its_prepared_changes() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("commit.db"), None).unwrap();
    let reader = Connection::open(registry.connection.database_path().unwrap()).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM queue_state")
        .unwrap();
    let control = StorageReadControl::with_limit(4_096);
    let transaction = registry.begin_with_control(&control).unwrap();
    transaction
        .append_entries(&[NotificationQueueEntry {
            sequence: 0,
            process_id: 7,
            channel: "events".into(),
            payload: "must roll back".into(),
        }])
        .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let (waiting, observed) = std::sync::mpsc::sync_channel(1);
    let commit_control = control.clone();
    let worker = std::thread::spawn(move || {
        control::observe_native_wait(waiting);
        send.send(transaction.commit_with_control(&commit_control))
            .unwrap();
    });
    notify_waiter(&control, &observed, &receive, worker);
    reader.execute_batch("COMMIT").unwrap();
    let transaction = registry.begin().unwrap();
    assert!(transaction.entries_from(0).unwrap().is_empty());
    transaction.commit().unwrap();
}

#[test]
fn authoritative_completion_is_not_revoked_by_the_original_query_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let registry =
        NotificationRegistry::open(&directory.path().join("completion.db"), None).unwrap();
    let control = StorageReadControl::with_limit(4_096);
    let transaction = registry.begin_with_control(&control).unwrap();
    transaction
        .append_entries(&[NotificationQueueEntry {
            sequence: 0,
            process_id: 7,
            channel: "events".into(),
            payload: "authoritative".into(),
        }])
        .unwrap();
    control.cancellation().cancel();
    transaction.commit().unwrap();
    let transaction = registry.begin().unwrap();
    assert_eq!(
        transaction.entries_from(0).unwrap()[0].payload,
        "authoritative"
    );
    transaction.commit().unwrap();
}

#[test]
fn native_vm_interruption_restores_the_lease_for_an_uncancelled_operation() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("native.db"), None).unwrap();
    let control = StorageReadControl::with_limit(4_096);
    let transaction = registry.begin_with_control(&control).unwrap();
    let cancel = control.cancellation().clone();
    transaction
        .connection
        .create_scalar_function(
            "cancel_registry_work",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |context| {
                let value: i64 = context.get(0)?;
                if value == 16 {
                    cancel.cancel();
                }
                Ok(value)
            },
        )
        .unwrap();
    {
        let _operation =
            control::operation(&transaction.connection, transaction.control.as_ref()).unwrap();
        let error = transaction.connection.query_row("WITH RECURSIVE n(value) AS (SELECT 1 UNION ALL SELECT value + 1 FROM n WHERE value < 10000000) SELECT sum(cancel_registry_work(value)) FROM n", [], |row| row.get::<_, i64>(0)).unwrap_err();
        assert!(matches!(
            registry_error("native cancellation fixture", &error),
            StorageBackendError::Cancelled(_)
        ));
    }
    assert!(!control::interrupted());
    drop(transaction);
    let lease = registry.connection.lease_connection().unwrap();
    assert!(lease.is_autocommit());
    assert_eq!(
        lease
            .pragma_query_value(None, "busy_timeout", |row| row.get::<_, u32>(0))
            .unwrap(),
        30_000
    );
    assert_eq!(
        lease
            .query_row("SELECT 73", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        73
    );
}

#[test]
fn nested_guards_restore_the_outer_signal_and_isolate_another_connection() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("nested.db"), None).unwrap();
    let connection = registry.connection.lease_connection().unwrap();
    let outer = StorageReadControl::with_limit(4_096);
    let inner = StorageReadControl::with_limit(4_096);
    let outer_guard = control::operation(&connection, Some(&outer)).unwrap();
    {
        let _inner_guard = control::operation(&connection, Some(&inner)).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    outer.cancellation().cancel();
    let independent = registry.begin().unwrap();
    assert_eq!(
        independent.queue_state().unwrap(),
        NotificationQueueState::default()
    );
    independent.commit().unwrap();
    let error = connection.query_row("WITH RECURSIVE n(value) AS (SELECT 1 UNION ALL SELECT value+1 FROM n WHERE value<10000000) SELECT sum(value) FROM n", [], |row| row.get::<_, i64>(0)).unwrap_err();
    assert!(matches!(
        registry_error("outer cancellation fixture", &error),
        StorageBackendError::Cancelled(_)
    ));
    drop(outer_guard);
    let other = registry.begin_with_control(&inner).unwrap();
    assert_eq!(
        other.queue_state().unwrap(),
        NotificationQueueState::default()
    );
    other.commit().unwrap();
}

#[test]
fn original_transaction_control_cannot_be_replaced_by_a_scan_control() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("retained.db"), None).unwrap();
    let original = StorageReadControl::with_limit(4_096);
    let transaction = registry.begin_with_control(&original).unwrap();
    original.cancellation().cancel();
    let mut called = false;
    let error = transaction
        .visit_entries_from(
            0,
            std::num::NonZeroUsize::MAX,
            &StorageReadControl::with_limit(4_096),
            &mut |_| {
                called = true;
                Ok(std::ops::ControlFlow::Continue(()))
            },
        )
        .unwrap_err();
    assert!(matches!(error, StorageBackendError::Cancelled(_)));
    assert!(!called);
    drop(transaction);
    registry.begin().unwrap().commit().unwrap();
}

#[test]
fn nested_native_work_keeps_every_enclosing_cancellation_signal() {
    let directory = tempfile::tempdir().unwrap();
    let registry =
        NotificationRegistry::open(&directory.path().join("inherited.db"), None).unwrap();
    let connection = registry.connection.lease_connection().unwrap();
    let first = StorageReadControl::with_limit(4_096);
    let second = StorageReadControl::with_limit(4_096);
    let third = StorageReadControl::with_limit(4_096);
    let _first_guard = control::operation(&connection, Some(&first)).unwrap();
    let _second_guard = control::operation(&connection, Some(&second)).unwrap();
    let _third_guard = control::operation(&connection, Some(&third)).unwrap();
    second.cancellation().cancel();
    let error = connection.query_row("WITH RECURSIVE n(value) AS (SELECT 1 UNION ALL SELECT value+1 FROM n WHERE value<10000000) SELECT sum(value) FROM n", [], |row| row.get::<_, i64>(0)).unwrap_err();
    assert!(matches!(
        registry_error("inherited cancellation fixture", &error),
        StorageBackendError::Cancelled(_)
    ));
    assert!(!first.cancellation().is_cancelled());
    assert!(!third.cancellation().is_cancelled());
}
