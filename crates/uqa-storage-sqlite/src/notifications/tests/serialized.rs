//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recover beside an active physical writer without committing its private records.

use super::*;
use uqa_storage::{
    notifications::{NotificationPublicationStore, PendingNotification},
    read_control::StorageReadControl,
};

#[test]
fn serialized_writer_replaces_only_acknowledged_publication_with_its_own_data_outcome() {
    for encrypted in [false, true] {
        for commit in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let database = directory.path().join("physical.db");
            let key = encrypted.then(|| StorageEncryptionKey::new("serialized notification key"));
            let connection = if encrypted {
                ManagedConnection::open_encrypted(&database, "serialized notification key").unwrap()
            } else {
                ManagedConnection::open(&database).unwrap()
            };
            let catalog = crate::Catalog::open(connection.clone()).unwrap();
            let recovery = connection.new_session();
            let recovery_catalog = crate::Catalog::open(recovery.clone()).unwrap();
            let registry = NotificationRegistry::open(&database, key.as_ref()).unwrap();
            let control = StorageReadControl::with_limit(1 << 20);
            let pending = [PendingNotification {
                channel: "events".into(),
                payload: "first".into(),
            }];
            connection.begin_transaction().unwrap();
            let mut first = registry.begin().unwrap();
            let original = first
                .prepare_publication(42, &pending, None, &control)
                .unwrap();
            connection
                .stage_notification_publication(&original)
                .unwrap();
            catalog.set_metadata("private", "first").unwrap();
            connection.commit_transaction().unwrap();
            drop(first);

            connection.begin_transaction().unwrap();
            catalog.set_metadata("private", "second").unwrap();
            let mut second = registry.begin_recovered(&recovery, &control).unwrap();
            assert_eq!(
                second.pending_acknowledgement(),
                Some(original.fingerprint())
            );
            assert_eq!(second.entries_from(0).unwrap()[0].payload, "first");
            assert_eq!(
                recovery_catalog.get_metadata("private").unwrap().as_deref(),
                Some("first")
            );
            let next = second
                .prepare_publication(
                    43,
                    &[PendingNotification {
                        channel: "events".into(),
                        payload: "second".into(),
                    }],
                    None,
                    &control,
                )
                .unwrap();
            connection
                .stage_notification_publication_after(&next, second.pending_acknowledgement())
                .unwrap();
            if commit {
                connection.commit_transaction().unwrap();
            } else {
                connection.rollback_transaction().unwrap();
            }
            drop(second);

            let completed = registry.begin_recovered(&recovery, &control).unwrap();
            let entries = completed.entries_from(0).unwrap();
            assert_eq!(entries.len(), if commit { 2 } else { 1 });
            assert_eq!(entries[0].process_id, 42);
            if commit {
                assert_eq!(entries[1].process_id, 43);
                assert_eq!(entries[1].payload, "second");
            }
            assert!(completed.pending_acknowledgement().is_none());
            recovery
                .visit_notification_publication(&control, &mut |publication| {
                    assert!(publication.is_none());
                    Ok(())
                })
                .unwrap();
            completed.commit().unwrap();
            assert_eq!(
                catalog.get_metadata("private").unwrap().as_deref(),
                Some(if commit { "second" } else { "first" })
            );
        }
    }
}
