//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::notifications::{
    NotificationPublication, NotificationPublicationStore, PendingNotification,
};

fn publication(payload: &str, sequence: u64) -> NotificationPublication {
    NotificationPublication::encode(
        [1; 16],
        sequence,
        sequence * 20,
        42,
        &[PendingNotification {
            channel: "events".into(),
            payload: payload.into(),
        }],
        &StorageReadControl::with_limit(4_096),
    )
    .unwrap()
}

fn pending(store: &dyn NotificationPublicationStore) -> Option<Vec<(String, String)>> {
    let mut result = None;
    store
        .visit_notification_publication(
            &StorageReadControl::with_limit(1 << 20),
            &mut |publication| {
                result = publication.map(|publication| {
                    publication
                        .messages()
                        .map(|message| {
                            let message = message.unwrap();
                            (message.channel.to_owned(), message.payload.to_owned())
                        })
                        .collect()
                });
                Ok(())
            },
        )
        .unwrap();
    result
}

#[test]
fn publication_shares_the_data_commit_and_fresh_reads_ignore_a_pinned_query_snapshot() {
    let persistence = Persistence::new();
    let writer = persistence.session(1 << 20);
    let listener = persistence.session(1 << 20);
    writer.begin_transaction().unwrap();
    listener.begin_read_transaction().unwrap();
    writer.put(b"row", b"committed").unwrap();
    writer
        .stage_notification_publication(&publication("one", 0))
        .unwrap();
    assert_eq!(pending(&listener), None);
    assert_eq!(listener.get(b"row").unwrap(), None);
    writer.commit_transaction().unwrap();
    assert_eq!(listener.get(b"row").unwrap(), None);
    assert_eq!(
        pending(&listener),
        Some(vec![("events".into(), "one".into())])
    );
    listener.rollback_transaction().unwrap();
    assert_eq!(
        listener.get(b"row").unwrap().as_deref(),
        Some(b"committed".as_slice())
    );
}

#[test]
fn read_only_publication_never_grants_user_write_access() {
    for serializable in [None, Some(false), Some(true)] {
        let persistence = Persistence::new();
        let store = persistence.session(1 << 20);
        store.begin_read_transaction().unwrap();
        if let Some(deferrable) = serializable {
            store
                .establish_serializable_snapshot_with(
                    SerializableSnapshotOptions {
                        read_only: true,
                        deferrable,
                    },
                    &mut |capture| capture(),
                )
                .unwrap();
        }
        store
            .stage_notification_publication(&publication("read only", 0))
            .unwrap();
        assert!(!store.transaction_has_written().unwrap());
        assert!(store.put(b"forbidden", b"write").is_err());
        store.commit_transaction().unwrap();
        assert_eq!(store.get(b"forbidden").unwrap(), None);
        assert_eq!(
            pending(&store),
            Some(vec![("events".into(), "read only".into())])
        );
    }
}

#[test]
fn retained_readers_can_recover_but_cannot_stage_or_acknowledge_publication() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    store.begin_transaction().unwrap();
    let retained = store
        .new_retained_read_session(&uqa_core::CancellationToken::new())
        .unwrap();
    let original = publication("original", 0);
    store.stage_notification_publication(&original).unwrap();
    store.commit_transaction().unwrap();
    assert_eq!(pending(&retained), pending(&store));
    assert!(retained.stage_notification_publication(&original).is_err());
    assert!(retained
        .acknowledge_notification_publication(
            original.fingerprint(),
            &StorageReadControl::with_limit(1 << 20)
        )
        .is_err());
    assert!(pending(&store).is_some());
}

#[test]
fn publication_cancellation_cannot_prevent_acknowledged_cleanup() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let original = publication("original", 0);
    store.begin_transaction().unwrap();
    store.stage_notification_publication(&original).unwrap();
    store.commit_transaction().unwrap();
    store.write_cancellation().unwrap().cancel();
    store.begin_read_transaction().unwrap();
    assert!(store
        .stage_notification_publication(&publication("cancelled", 1))
        .is_err());
    assert!(!store.transaction_has_written().unwrap());
    store.rollback_transaction().unwrap();
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    let mut visited = false;
    assert!(store
        .visit_notification_publication(&cancelled, &mut |_| {
            visited = true;
            Ok(())
        })
        .is_err());
    assert!(!visited);
    store
        .acknowledge_notification_publication(
            original.fingerprint(),
            &StorageReadControl::with_limit(1 << 20),
        )
        .unwrap();
    assert_eq!(pending(&store), None);
}

#[test]
fn publication_savepoint_and_transaction_rollback_restore_the_original_intent() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    store.begin_transaction().unwrap();
    store.savepoint("empty").unwrap();
    store
        .stage_notification_publication(&publication("discard", 0))
        .unwrap();
    store.rollback_to_savepoint("empty").unwrap();
    store.commit_transaction().unwrap();
    assert_eq!(pending(&store), None);
    store.begin_transaction().unwrap();
    store
        .stage_notification_publication(&publication("original", 0))
        .unwrap();
    store.savepoint("original").unwrap();
    store
        .stage_notification_publication(&publication("discard", 0))
        .unwrap();
    store.rollback_to_savepoint("original").unwrap();
    store.commit_transaction().unwrap();
    assert_eq!(
        pending(&store),
        Some(vec![("events".into(), "original".into())])
    );
    let next = persistence.session(1 << 20);
    next.begin_transaction().unwrap();
    next.stage_notification_publication(&publication("aborted", 1))
        .unwrap();
    next.rollback_transaction().unwrap();
    assert_eq!(pending(&next), pending(&store));
}

#[test]
fn pending_publication_cannot_be_overwritten_or_cleared_by_another_fingerprint() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let original = publication("original", 0);
    store.begin_transaction().unwrap();
    store.stage_notification_publication(&original).unwrap();
    store.commit_transaction().unwrap();
    let other = publication("other", 1);
    store
        .acknowledge_notification_publication(
            other.fingerprint(),
            &StorageReadControl::with_limit(1 << 20),
        )
        .unwrap();
    store.begin_transaction().unwrap();
    store.put(b"row", b"must not commit").unwrap();
    store.stage_notification_publication(&other).unwrap();
    assert!(store.commit_transaction().is_err());
    store.rollback_transaction().unwrap();
    assert_eq!(store.get(b"row").unwrap(), None);
    assert_eq!(
        pending(&store),
        Some(vec![("events".into(), "original".into())])
    );
    store
        .acknowledge_notification_publication(
            original.fingerprint(),
            &StorageReadControl::with_limit(1 << 20),
        )
        .unwrap();
    assert_eq!(pending(&store), None);
}

#[test]
fn a_long_read_only_transaction_uses_the_latest_cleared_slot_revision() {
    let persistence = Persistence::new();
    let first = persistence.session(1 << 20);
    let original = publication("original", 0);
    first.begin_transaction().unwrap();
    first.stage_notification_publication(&original).unwrap();
    first.commit_transaction().unwrap();
    let later = persistence.session(1 << 20);
    later.begin_read_transaction().unwrap();
    first
        .acknowledge_notification_publication(
            original.fingerprint(),
            &StorageReadControl::with_limit(1 << 20),
        )
        .unwrap();
    assert_eq!(pending(&later), None);
    later
        .stage_notification_publication(&publication("later", 1))
        .unwrap();
    later.commit_transaction().unwrap();
    assert_eq!(
        pending(&first),
        Some(vec![("events".into(), "later".into())])
    );
}

#[test]
fn publication_retry_preserves_the_original_evaluated_batch_and_outcome() {
    for fault in [
        CommitFault::LoseBeforeCommit,
        CommitFault::LoseReply,
        CommitFault::ConcurrentCommit,
    ] {
        let persistence = Persistence::new();
        let store = persistence.session(1 << 20);
        store.begin_transaction().unwrap();
        store.put(b"row", b"evaluated once").unwrap();
        let original = publication("evaluated once", 0);
        store.stage_notification_publication(&original).unwrap();
        persistence.state.lock().commit_fault = fault;
        let result = store.commit_transaction();
        if fault == CommitFault::ConcurrentCommit {
            result.unwrap();
        } else {
            assert!(result.is_err());
            assert_eq!(pending(&store).is_some(), fault == CommitFault::LoseReply);
            store.stage_notification_publication(&original).unwrap();
            store.commit_transaction().unwrap();
        }
        assert_eq!(
            store.get(b"row").unwrap().as_deref(),
            Some(b"evaluated once".as_slice())
        );
        assert_eq!(
            pending(&store),
            Some(vec![("events".into(), "evaluated once".into())])
        );
        let state = persistence.state.lock();
        let expected_attempts = if fault == CommitFault::LoseReply {
            1
        } else {
            2
        };
        assert_eq!(state.attempts.len(), expected_attempts);
        assert!(state
            .attempts
            .iter()
            .all(|fingerprint| *fingerprint == state.attempts[0]));
    }
}

#[test]
fn lost_acknowledgement_reply_does_not_republish_or_clear_a_later_intent() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let original = publication("original", 0);
    let control = StorageReadControl::with_limit(1 << 20);
    store.begin_transaction().unwrap();
    store.stage_notification_publication(&original).unwrap();
    store.commit_transaction().unwrap();
    persistence.state.lock().commit_fault = CommitFault::LoseReply;
    assert!(store
        .acknowledge_notification_publication(original.fingerprint(), &control)
        .is_err());
    assert_eq!(pending(&store), None);
    store.begin_transaction().unwrap();
    store
        .stage_notification_publication(&publication("later", 1))
        .unwrap();
    store.commit_transaction().unwrap();
    store
        .acknowledge_notification_publication(original.fingerprint(), &control)
        .unwrap();
    assert_eq!(
        pending(&store),
        Some(vec![("events".into(), "later".into())])
    );
}
