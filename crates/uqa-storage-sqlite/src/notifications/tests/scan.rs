//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{num::NonZeroUsize, ops::ControlFlow};

use uqa_storage::{read_control::StorageReadControl, StorageBackendError};

use super::{NotificationQueueEntry, NotificationRegistry};

fn populate(registry: &NotificationRegistry) {
    let transaction = registry.begin().unwrap();
    transaction
        .append_entries(
            &(0..7)
                .map(|sequence| NotificationQueueEntry {
                    sequence,
                    process_id: 42,
                    channel: "events".into(),
                    payload: format!("opaque {sequence}\n\"value\""),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    transaction.commit().unwrap();
}

#[test]
fn bounded_borrowed_scans_resume_without_skipping_declined_rows() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("database"), None).unwrap();
    populate(&registry);
    let transaction = registry.begin().unwrap();
    let control = StorageReadControl::with_limit(0);
    let limit = NonZeroUsize::new(3).unwrap();
    let mut seen = Vec::new();
    let first = transaction
        .visit_entries_from(0, limit, &control, &mut |entry| {
            assert_eq!(entry.process_id, 42);
            assert_eq!(entry.channel, "events");
            assert_eq!(
                entry.payload,
                format!("opaque {}\n\"value\"", entry.sequence)
            );
            seen.push(entry.sequence);
            Ok(ControlFlow::Continue(()))
        })
        .unwrap();
    assert_eq!(seen, [0, 1, 2]);
    assert_eq!(first.visited, 3);
    assert_eq!(first.next_sequence, 3);
    assert!(!first.exhausted);

    let stopped = transaction
        .visit_entries_from(first.next_sequence, limit, &control, &mut |entry| {
            if entry.sequence == 4 {
                return Ok(ControlFlow::Break(()));
            }
            seen.push(entry.sequence);
            Ok(ControlFlow::Continue(()))
        })
        .unwrap();
    assert_eq!(stopped.visited, 2);
    assert_eq!(stopped.next_sequence, 4);
    assert!(!stopped.exhausted);

    let last = transaction
        .visit_entries_from(
            stopped.next_sequence,
            NonZeroUsize::new(4).unwrap(),
            &control,
            &mut |entry| {
                seen.push(entry.sequence);
                Ok(ControlFlow::Continue(()))
            },
        )
        .unwrap();
    assert_eq!(seen, [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(last.next_sequence, 7);
    assert!(last.exhausted);
    assert_eq!(transaction.entries_from(0).unwrap().len(), 7);
    transaction.commit().unwrap();
}

#[test]
fn borrowed_scans_stop_actual_work_on_cancellation_and_keep_consumer_errors() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("database"), None).unwrap();
    populate(&registry);
    let transaction = registry.begin().unwrap();
    let control = StorageReadControl::with_limit(0);
    let mut calls = 0;
    let result = transaction.visit_entries_from(0, NonZeroUsize::MAX, &control, &mut |_| {
        calls += 1;
        control.cancellation().cancel();
        Ok(ControlFlow::Continue(()))
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    assert_eq!(calls, 1);
    let result = transaction.visit_entries_from(0, NonZeroUsize::MAX, &control, &mut |_| {
        panic!("cancelled scan must not invoke its consumer")
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));

    let control = StorageReadControl::with_limit(0);
    let result = transaction.visit_entries_from(0, NonZeroUsize::MAX, &control, &mut |_| {
        control.cancellation().cancel();
        Err(StorageBackendError::Other(
            "original consumer failure".into(),
        ))
    });
    assert!(
        matches!(result, Err(StorageBackendError::Other(message)) if message == "original consumer failure")
    );
    assert_eq!(transaction.entries_from(0).unwrap().len(), 7);
    transaction.commit().unwrap();
}

#[test]
fn borrowed_scan_limits_maximum_payload_visits_and_preserves_empty_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("database"), None).unwrap();
    let transaction = registry.begin().unwrap();
    let entry = NotificationQueueEntry {
        sequence: 0,
        process_id: 42,
        channel: "c".repeat(63),
        payload: "p".repeat(7_999),
    };
    for sequence in 0..32 {
        transaction
            .append_entries(&[NotificationQueueEntry {
                sequence,
                ..entry.clone()
            }])
            .unwrap();
    }
    let control = StorageReadControl::with_limit(0);
    let mut calls = 0;
    let progress = transaction
        .visit_entries_from(0, NonZeroUsize::new(2).unwrap(), &control, &mut |value| {
            assert_eq!(value.channel, entry.channel);
            assert_eq!(value.payload, entry.payload);
            calls += 1;
            Ok(ControlFlow::Continue(()))
        })
        .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(progress.next_sequence, 2);
    assert!(!progress.exhausted);
    assert_eq!(control.memory().used(), 0);

    let declined = transaction
        .visit_entries_from(2, NonZeroUsize::MAX, &control, &mut |_| {
            Ok(ControlFlow::Break(()))
        })
        .unwrap();
    assert_eq!(declined.visited, 1);
    assert_eq!(declined.next_sequence, 2);
    assert!(!declined.exhausted);
    let empty = transaction
        .visit_entries_from(32, NonZeroUsize::MAX, &control, &mut |_| {
            panic!("empty scan must not invoke its consumer")
        })
        .unwrap();
    assert_eq!(empty.visited, 0);
    assert_eq!(empty.next_sequence, 32);
    assert!(empty.exhausted);
    transaction.commit().unwrap();
}

#[test]
fn malformed_registry_text_fails_before_consumer_delivery() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("database"), None).unwrap();
    populate(&registry);
    let transaction = registry.begin().unwrap();
    transaction
        .connection
        .execute(
            "UPDATE queue_entries SET payload = CAST(x'80' AS TEXT) WHERE sequence = 0",
            [],
        )
        .unwrap();
    let result = transaction.visit_entries_from(
        0,
        NonZeroUsize::MAX,
        &StorageReadControl::with_limit(0),
        &mut |_| panic!("invalid text must not be delivered"),
    );
    assert!(
        matches!(result, Err(StorageBackendError::Other(message)) if message.contains("read queue entry payload"))
    );
}
