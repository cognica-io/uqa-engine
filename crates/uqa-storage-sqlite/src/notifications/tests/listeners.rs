//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata-only coordination and bounded local channel decoding.

use super::{NotificationListenerRow, NotificationRegistry};
use uqa_storage::{
    notifications::NotificationListenerKey, read_control::StorageReadControl, StorageBackendError,
};

fn listener(owner: u8, session_id: u64) -> NotificationListenerRow {
    NotificationListenerRow {
        owner_id: [owner; 16],
        session_id,
        process_id: 42,
        wake_port: 1234,
        channels: vec!["events".into()],
        transaction_open: true,
        next_sequence: 7,
        position: 120,
    }
}

#[test]
fn metadata_resumes_after_deleted_keys_without_reading_channel_json() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("database"), None).unwrap();
    let transaction = registry.begin().unwrap();
    for (owner, session) in [(2, 256), (1, u64::MAX), (2, 0), (1, 1)] {
        transaction
            .save_listener(&listener(owner, session))
            .unwrap();
    }
    transaction
        .connection
        .execute("UPDATE listeners SET channels_json = '['", [])
        .unwrap();
    // Explicit full materialization still diagnoses invalid channel JSON.
    assert!(transaction.listeners().is_err());
    let mut after = None;
    for (owner, session_id) in [(1, 1), (1, u64::MAX), (2, 0), (2, 256)] {
        let row = transaction.listener_metadata_after(after).unwrap().unwrap();
        assert_eq!(
            row.key,
            NotificationListenerKey {
                owner_id: [owner; 16],
                session_id
            }
        );
        assert_eq!(
            (
                row.process_id,
                row.wake_port,
                row.transaction_open,
                row.next_sequence,
                row.position
            ),
            (42, 1234, true, 7, 120)
        );
        transaction
            .drop_listener(row.key.owner_id, row.key.session_id)
            .unwrap();
        after = Some(row.key);
    }
    assert!(transaction
        .listener_metadata_after(after)
        .unwrap()
        .is_none());
    assert!(transaction.listener_metadata_after(None).unwrap().is_none());
    transaction.commit().unwrap();
}

#[test]
fn local_lookup_bounds_count_text_and_encoded_input_without_hydrating_neighbors() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("database"), None).unwrap();
    let transaction = registry.begin().unwrap();
    let key = NotificationListenerKey {
        owner_id: [1; 16],
        session_id: 1,
    };
    transaction.save_listener(&listener(1, 1)).unwrap();
    transaction.save_listener(&listener(2, 1)).unwrap();
    transaction
        .connection
        .execute(
            "UPDATE listeners SET channels_json = ?1 WHERE owner_id = ?2",
            rusqlite::params!["[".repeat(1_048_576), [2_u8; 16].as_slice()],
        )
        .unwrap();
    assert_eq!(
        transaction.listener(key, Some(1)).unwrap(),
        Some(listener(1, 1))
    );
    assert!(transaction
        .listener(
            NotificationListenerKey {
                owner_id: [3; 16],
                session_id: 0
            },
            Some(0)
        )
        .unwrap()
        .is_none());
    for channels in [
        vec![],
        vec!["한".repeat(21)],
        vec!["\u{1}".repeat(63)],
        vec!["a".into(), "b".into()],
    ] {
        let row = NotificationListenerRow {
            channels,
            ..listener(1, 1)
        };
        transaction.save_listener(&row).unwrap();
        assert_eq!(
            transaction.listener(key, Some(row.channels.len())).unwrap(),
            Some(row.clone())
        );
        assert_eq!(
            transaction.listener(key, Some(usize::MAX)).unwrap(),
            Some(row)
        );
    }
    for json in [
        "[\"a\",\"b\"]".to_owned(),
        serde_json::to_string(&vec!["한".repeat(22)]).unwrap(),
        serde_json::to_string(&vec!["x".repeat(384)]).unwrap(),
        "[\"a\"] true".into(),
        "[{}]".into(),
        "[1]".into(),
        "null".into(),
    ] {
        transaction
            .connection
            .execute(
                "UPDATE listeners SET channels_json = ?1 WHERE owner_id = ?2",
                rusqlite::params![json, key.owner_id.as_slice()],
            )
            .unwrap();
        assert!(
            transaction.listener(key, Some(1)).is_err(),
            "invalid retained channels accepted"
        );
    }
    let legacy = NotificationListenerRow {
        channels: vec!["x".repeat(64); 2],
        ..listener(1, 1)
    };
    transaction.save_listener(&legacy).unwrap();
    assert_eq!(transaction.listener(key, None).unwrap(), Some(legacy));
    transaction.commit().unwrap();
}

#[test]
fn metadata_and_local_lookup_keep_original_transaction_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("database"), None).unwrap();
    let control = StorageReadControl::with_limit(0);
    let transaction = registry.begin_with_control(&control).unwrap();
    transaction.save_listener(&listener(1, u64::MAX)).unwrap();
    let key = transaction
        .listener_metadata_after(None)
        .unwrap()
        .unwrap()
        .key;
    assert!(transaction.listener(key, Some(1)).unwrap().is_some());
    control.cancellation().cancel();
    assert!(matches!(
        transaction.listener_metadata_after(Some(key)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        transaction.listener(key, Some(1)),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(transaction);
    assert!(registry
        .begin()
        .unwrap()
        .listener_metadata_after(None)
        .unwrap()
        .is_none());
}
