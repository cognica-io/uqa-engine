//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{
    notifications::{NotificationPublication, NotificationPublicationStart, PendingNotification},
    read_control::StorageReadControl,
    PersistentStorageProvider, PersistentStorageSession,
};

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1 << 20)
}

fn provider(path: &std::path::Path, native: bool) -> Box<dyn PersistentStorageProvider> {
    let connection = ManagedConnection::open(path).unwrap();
    if native {
        Box::new(crate::SQLiteStorageProvider::new(connection))
    } else {
        Box::new(crate::SQLiteKeyValueStorage::from_connection(connection).unwrap())
    }
}

fn pending() -> [PendingNotification; 1] {
    [PendingNotification {
        channel: "events".into(),
        payload: "original payload 한글".into(),
    }]
}

fn listener() -> NotificationListenerRow {
    NotificationListenerRow {
        owner_id: [1; 16],
        session_id: 9,
        process_id: 42,
        wake_port: 1234,
        channels: vec!["events".into()],
        transaction_open: false,
        next_sequence: 0,
        position: 0,
    }
}

fn is_pending(session: &PersistentStorageSession) -> bool {
    let mut pending = false;
    session
        .backend
        .notification_publications()
        .unwrap()
        .visit_notification_publication(&control(), &mut |publication| {
            pending = publication.is_some();
            Ok(())
        })
        .unwrap();
    pending
}

#[test]
fn idle_poll_reads_committed_work_while_another_registry_writer_is_admitted() {
    for native in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("polling.db");
        let provider = provider(&database, native);
        let session = provider.open_session().unwrap();
        let store = session.backend.notification_publications().unwrap();
        let registry = NotificationRegistry::open(&database, None).unwrap();
        let row = listener();
        let transaction = registry.begin().unwrap();
        transaction.save_listener(&row).unwrap();
        transaction.commit().unwrap();

        let writer = registry.begin().unwrap();
        writer
            .append_entries(&[NotificationQueueEntry {
                sequence: 0,
                process_id: 43,
                channel: "events".into(),
                payload: "committed only".into(),
            }])
            .unwrap();
        writer
            .save_queue_state(NotificationQueueState {
                next_sequence: 1,
                head_position: 32,
            })
            .unwrap();
        assert!(!registry
            .poll_needed(store, &[row.owner_id], &control())
            .unwrap());
        writer.commit().unwrap();
        assert!(registry
            .poll_needed(store, &[row.owner_id], &control())
            .unwrap());
        assert!(!registry.poll_needed(store, &[[2; 16]], &control()).unwrap());
        assert!(!registry.poll_needed(store, &[], &control()).unwrap());

        let transaction = registry.begin().unwrap();
        let mut row = row;
        row.transaction_open = true;
        transaction.save_listener(&row).unwrap();
        transaction.commit().unwrap();
        assert!(!registry
            .poll_needed(store, &[row.owner_id], &control())
            .unwrap());
    }
}

#[test]
fn polling_without_local_listeners_still_detects_committed_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("cleanup.db");
    let provider = provider(&database, true);
    let session = provider.open_session().unwrap();
    let store = session.backend.notification_publications().unwrap();
    let registry = NotificationRegistry::open(&database, None).unwrap();
    session.backend.begin_transaction().unwrap();
    let mut prepared = registry.begin().unwrap();
    let publication = prepared
        .prepare_publication(42, &pending(), None, &control())
        .unwrap();
    store.stage_notification_publication(&publication).unwrap();
    session.backend.commit_transaction().unwrap();
    drop(prepared);
    assert!(registry.poll_needed(store, &[], &control()).unwrap());
    registry
        .begin_recovered(store, &control())
        .unwrap()
        .commit()
        .unwrap();
    assert!(!registry.poll_needed(store, &[], &control()).unwrap());
}

#[test]
fn unresolved_publication_releases_the_registry_writer_and_resumes_its_original_range() {
    for native in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("reserved-publication.db");
        let provider = provider(&database, native);
        let session = provider.open_session().unwrap();
        let store = session.backend.notification_publications().unwrap();
        let registry = NotificationRegistry::open(&database, None).unwrap();
        let owner = [9; 16];
        session.backend.begin_transaction().unwrap();
        let mut original = registry
            .begin_publishing(store, &control(), None, &mut |_| Ok(true))
            .unwrap();
        let publication = original
            .prepare_publication(42, &pending(), None, &control())
            .unwrap();
        store.stage_notification_publication(&publication).unwrap();
        original.suspend_publication(&publication, owner).unwrap();

        let consumer = registry.begin_recovered(store, &control()).unwrap();
        assert_eq!(consumer.queue_state().unwrap().next_sequence, 0);
        assert!(consumer.entries_from(0).unwrap().is_empty());
        consumer.allocate_backend_process_id().unwrap();
        consumer.commit().unwrap();

        let waiting = control();
        let error = registry
            .begin_publishing(store, &waiting, None, &mut |_| {
                waiting.cancellation().cancel();
                Ok(true)
            })
            .err()
            .expect("a live reservation must block another publisher");
        assert!(matches!(error, StorageBackendError::Cancelled(_)));

        let mut resumed = registry
            .begin_publishing(store, &control(), Some((&publication, owner)), &mut |_| {
                Ok(true)
            })
            .unwrap();
        assert!(!resumed
            .resume_publication(&publication, owner, &control())
            .unwrap());
        session.backend.commit_transaction().unwrap();
        resumed.commit().unwrap();
        let consumer = registry.begin_recovered(store, &control()).unwrap();
        assert_eq!(consumer.entries_from(0).unwrap().len(), 1);
        assert_eq!(consumer.queue_state().unwrap().next_sequence, 1);
        consumer.commit().unwrap();
        assert!(!is_pending(&session));
    }
}

#[test]
fn independent_recovery_allows_later_publications_before_original_sender_resolution() {
    for native in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("recovered-reservation.db");
        let provider = provider(&database, native);
        let session = provider.open_session().unwrap();
        let store = session.backend.notification_publications().unwrap();
        let registry = NotificationRegistry::open(&database, None).unwrap();
        let owner = [9; 16];
        session.backend.begin_transaction().unwrap();
        let mut original = registry
            .begin_publishing(store, &control(), None, &mut |_| Ok(true))
            .unwrap();
        let publication = original
            .prepare_publication(42, &pending(), None, &control())
            .unwrap();
        store.stage_notification_publication(&publication).unwrap();
        session.backend.commit_transaction().unwrap();
        original.suspend_publication(&publication, owner).unwrap();
        registry
            .begin_recovered(store, &control())
            .unwrap()
            .commit()
            .unwrap();

        session.backend.begin_transaction().unwrap();
        let mut later = registry
            .begin_publishing(store, &control(), None, &mut |_| Ok(true))
            .unwrap();
        let second = later
            .prepare_publication(43, &pending(), None, &control())
            .unwrap();
        store.stage_notification_publication(&second).unwrap();
        session.backend.commit_transaction().unwrap();
        later.commit().unwrap();
        let mut resumed = registry
            .begin_publishing(store, &control(), Some((&publication, owner)), &mut |_| {
                Ok(true)
            })
            .unwrap();
        assert!(resumed
            .resume_publication(&publication, owner, &control())
            .unwrap());
        let entries = resumed.entries_from(0).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.process_id)
                .collect::<Vec<_>>(),
            [42, 43]
        );
        resumed.commit().unwrap();
        assert!(!is_pending(&session));
    }
}

#[test]
fn abandoned_reservation_is_discarded_only_after_committed_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("abandoned-reservation.db");
    let provider = provider(&database, true);
    let session = provider.open_session().unwrap();
    let store = session.backend.notification_publications().unwrap();
    let registry = NotificationRegistry::open(&database, None).unwrap();
    session.backend.begin_transaction().unwrap();
    let mut original = registry
        .begin_publishing(store, &control(), None, &mut |_| Ok(true))
        .unwrap();
    let publication = original
        .prepare_publication(42, &pending(), None, &control())
        .unwrap();
    store.stage_notification_publication(&publication).unwrap();
    original.suspend_publication(&publication, [9; 16]).unwrap();
    session.backend.rollback_transaction().unwrap();
    let next = registry
        .begin_publishing(store, &control(), None, &mut |_| Ok(false))
        .unwrap();
    assert_eq!(next.queue_state().unwrap().next_sequence, 0);
    assert!(next.entries_from(0).unwrap().is_empty());
    next.commit().unwrap();
}

#[test]
fn committed_data_recovers_exactly_one_publication_on_both_sides_of_registry_commit() {
    for native in [false, true] {
        for registry_committed in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let database = directory.path().join("recovery.db");
            let provider = provider(&database, native);
            let sender = provider.open_session().unwrap();
            let listener_session = provider.open_session().unwrap();
            let registry = NotificationRegistry::open(&database, None).unwrap();
            sender.backend.begin_transaction().unwrap();
            sender.catalog.set_metadata("committed_row", "one").unwrap();
            let mut transaction = registry.begin().unwrap();
            let publication = transaction
                .prepare_publication(42, &pending(), Some(&listener()), &control())
                .unwrap();
            sender
                .backend
                .notification_publications()
                .unwrap()
                .stage_notification_publication(&publication)
                .unwrap();
            assert!(!is_pending(&listener_session));
            sender.backend.commit_transaction().unwrap();
            if registry_committed {
                transaction.commit().unwrap();
            } else {
                drop(transaction);
            }
            drop(sender);
            assert_eq!(
                listener_session
                    .catalog
                    .get_metadata("committed_row")
                    .unwrap()
                    .as_deref(),
                Some("one")
            );
            assert!(is_pending(&listener_session));
            let recovered = registry
                .begin_recovered(
                    listener_session
                        .backend
                        .notification_publications()
                        .unwrap(),
                    &control(),
                )
                .unwrap();
            assert!(!is_pending(&listener_session));
            let entries = recovered.entries_from(0).unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].sequence, 0);
            assert_eq!(entries[0].process_id, 42);
            assert_eq!(entries[0].channel, "events");
            assert_eq!(entries[0].payload, "original payload 한글");
            assert_eq!(recovered.listeners().unwrap(), vec![listener()]);
            recovered.commit().unwrap();
            let repeated = registry
                .begin_recovered(
                    listener_session
                        .backend
                        .notification_publications()
                        .unwrap(),
                    &control(),
                )
                .unwrap();
            assert_eq!(repeated.entries_from(0).unwrap(), entries);
            repeated.commit().unwrap();
        }
    }
}

#[test]
fn read_only_subscription_commits_recover_without_messages_and_rollback_stays_invisible() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("subscription.db");
    let provider = provider(&database, true);
    let sender = provider.open_session().unwrap();
    let registry = NotificationRegistry::open(&database, None).unwrap();
    for (publication_sequence, listening) in [true, true, false].into_iter().enumerate() {
        let mut final_listener = listener();
        if !listening {
            final_listener.channels.clear();
        }
        sender.backend.begin_read_transaction().unwrap();
        let mut transaction = registry
            .begin_recovered(
                sender.backend.notification_publications().unwrap(),
                &control(),
            )
            .unwrap();
        let publication = transaction
            .prepare_publication(42, &[], Some(&final_listener), &control())
            .unwrap();
        assert_eq!(
            publication.header().publication_sequence,
            publication_sequence as u64
        );
        assert_eq!(publication.header().next_sequence, 0);
        sender
            .backend
            .notification_publications()
            .unwrap()
            .stage_notification_publication(&publication)
            .unwrap();
        assert!(!sender.backend.transaction_has_written().unwrap());
        sender.backend.commit_transaction().unwrap();
        drop(transaction);
        let recovered = registry
            .begin_recovered(
                sender.backend.notification_publications().unwrap(),
                &control(),
            )
            .unwrap();
        assert_eq!(recovered.listeners().unwrap().len(), usize::from(listening));
        assert!(recovered.entries_from(0).unwrap().is_empty());
        recovered.commit().unwrap();
    }
    sender.backend.begin_read_transaction().unwrap();
    let mut transaction = registry.begin().unwrap();
    let publication = transaction
        .prepare_publication(42, &[], Some(&listener()), &control())
        .unwrap();
    sender
        .backend
        .notification_publications()
        .unwrap()
        .stage_notification_publication(&publication)
        .unwrap();
    sender.backend.rollback_transaction().unwrap();
    drop(transaction);
    let recovered = registry
        .begin_recovered(
            sender.backend.notification_publications().unwrap(),
            &control(),
        )
        .unwrap();
    assert!(recovered.listeners().unwrap().is_empty());
    assert!(!is_pending(&sender));
    recovered.commit().unwrap();
}

#[test]
fn publication_rejects_changed_payload_or_incarnation_without_mutating_committed_state() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("identity.db");
    let registry = NotificationRegistry::open(&database, None).unwrap();
    let mut transaction = registry.begin().unwrap();
    let original = transaction
        .prepare_publication(42, &pending(), None, &control())
        .unwrap();
    let header = original.header();
    transaction.commit().unwrap();
    let mut transaction = registry.begin().unwrap();
    for registry_id in [header.registry_id, [2; 16]] {
        let changed = NotificationPublication::encode(
            NotificationPublicationStart {
                registry_id,
                publication_sequence: header.publication_sequence,
                first_sequence: header.first_sequence,
                first_position: header.first_position,
                process_id: header.process_id,
            },
            &[PendingNotification {
                channel: "events".into(),
                payload: "changed".into(),
            }],
            None,
            &control(),
        )
        .unwrap();
        assert!(transaction
            .apply_publication(changed.view(), &control())
            .is_err());
    }
    transaction
        .apply_publication(original.view(), &control())
        .unwrap();
    assert_eq!(transaction.entries_from(0).unwrap().len(), 1);
    assert_eq!(
        transaction.entries_from(0).unwrap()[0].payload,
        pending()[0].payload
    );
    transaction.commit().unwrap();
}

#[test]
fn failed_queue_append_rolls_back_its_subscription_and_acknowledgement() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("atomic.db"), None).unwrap();
    let mut transaction = registry.begin().unwrap();
    transaction.connection.execute_batch("CREATE TEMP TRIGGER reject_notification BEFORE INSERT ON queue_entries BEGIN SELECT RAISE(ABORT, 'injected append rejection'); END;").unwrap();
    assert!(transaction
        .prepare_publication(42, &pending(), Some(&listener()), &control())
        .is_err());
    assert!(transaction.listeners().unwrap().is_empty());
    assert!(transaction.entries_from(0).unwrap().is_empty());
    assert_eq!(
        transaction.queue_state().unwrap(),
        NotificationQueueState::default()
    );
    transaction
        .connection
        .execute_batch("DROP TRIGGER reject_notification")
        .unwrap();
    let publication = transaction
        .prepare_publication(42, &pending(), Some(&listener()), &control())
        .unwrap();
    assert_eq!(publication.header().publication_sequence, 0);
    transaction.commit().unwrap();
}

#[test]
fn failed_publication_rollback_prevents_registry_commit() {
    let directory = tempfile::tempdir().unwrap();
    let registry = NotificationRegistry::open(&directory.path().join("aborted.db"), None).unwrap();
    let mut transaction = registry.begin().unwrap();
    transaction.connection.execute_batch("CREATE TEMP TRIGGER abort_notification BEFORE INSERT ON queue_entries BEGIN SELECT RAISE(ROLLBACK, 'injected transaction abort'); END;").unwrap();
    let error = transaction
        .prepare_publication(42, &pending(), Some(&listener()), &control())
        .err()
        .expect("the injected abort must reject publication");
    assert!(error
        .to_string()
        .contains("rollback notification publication failed"));
    assert!(transaction
        .commit()
        .unwrap_err()
        .to_string()
        .contains("cannot commit"));
    let transaction = registry.begin().unwrap();
    assert!(transaction.listeners().unwrap().is_empty());
    assert!(transaction.entries_from(0).unwrap().is_empty());
    assert_eq!(
        transaction.queue_state().unwrap(),
        NotificationQueueState::default()
    );
    transaction.commit().unwrap();
}
