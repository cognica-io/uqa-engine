//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared acceptance schedules for disposable provider sessions with notification publication.

use super::{
    NotificationPublication, NotificationPublicationStart, NotificationPublicationStore,
    PendingNotification,
};
use crate::{
    read_control::StorageReadControl, PersistentStorageSession, StorageBackendError,
    StorageBackendResult, StorageSavepointId,
};

const DATA_KEY: &str = "notification-conformance-data";
const PRIVATE_KEY: &str = "notification-conformance-private";

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1 << 20)
}

fn publication(sequence: u64) -> StorageBackendResult<NotificationPublication> {
    NotificationPublication::encode(
        NotificationPublicationStart {
            registry_id: [7; 16],
            publication_sequence: sequence,
            first_sequence: sequence,
            first_position: sequence * 8_192,
            process_id: 42,
        },
        &[
            PendingNotification {
                channel: "events".into(),
                payload: format!("original notification {sequence}: 한글 \\ \" \n"),
            },
            PendingNotification {
                channel: "other".into(),
                payload: String::new(),
            },
        ],
        None,
        &control(),
    )
    .map_err(crate::mvcc::VersionError::into_storage_error)
}

fn store(
    session: &PersistentStorageSession,
) -> StorageBackendResult<&dyn NotificationPublicationStore> {
    session.backend.notification_publications().ok_or_else(|| {
        StorageBackendError::Other("provider has no notification publication capability".into())
    })
}

fn assert_pending(
    session: &PersistentStorageSession,
    expected: Option<&NotificationPublication>,
) -> StorageBackendResult<()> {
    store(session)?.visit_notification_publication(&control(), &mut |actual| {
        assert_eq!(actual.is_some(), expected.is_some());
        if let (Some(actual), Some(expected)) = (actual, expected) {
            assert_eq!(actual.header(), expected.header());
            assert_eq!(actual.fingerprint(), expected.fingerprint());
            let messages = actual
                .messages()
                .collect::<Result<Vec<_>, _>>()
                .map_err(crate::mvcc::VersionError::into_storage_error)?;
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].channel, "events");
            assert_eq!(
                messages[0].payload,
                format!(
                    "original notification {}: 한글 \\ \" \n",
                    expected.header().first_sequence
                )
            );
            assert_eq!(messages[1].channel, "other");
            assert!(messages[1].payload.is_empty());
        }
        Ok(())
    })
}

/// Verify atomic data/publication commit, rollback and savepoint undo, and fresh auxiliary reads alongside a pinned query snapshot. Close both disposable sessions before calling [`verify_publication_reopen`].
pub fn verify_publication_sessions(
    writer: &PersistentStorageSession,
    reader: &PersistentStorageSession,
) -> StorageBackendResult<()> {
    writer.catalog.set_metadata(DATA_KEY, "before")?;
    let original = publication(0)?;
    writer.backend.begin_transaction()?;
    writer.catalog.set_metadata(DATA_KEY, "rollback")?;
    store(writer)?.stage_notification_publication(&original)?;
    writer.backend.rollback_transaction()?;
    assert_pending(reader, None)?;
    assert_eq!(
        reader.catalog.get_metadata(DATA_KEY)?.as_deref(),
        Some("before")
    );

    writer.backend.begin_transaction()?;
    let savepoint = StorageSavepointId::allocate();
    writer.backend.savepoint(savepoint)?;
    store(writer)?.stage_notification_publication(&original)?;
    writer.backend.rollback_to_savepoint(savepoint)?;
    writer.backend.commit_transaction()?;
    assert_pending(reader, None)?;

    reader.backend.begin_read_transaction()?;
    assert_eq!(
        reader.catalog.get_metadata(DATA_KEY)?.as_deref(),
        Some("before")
    );
    writer.backend.begin_transaction()?;
    writer.catalog.set_metadata(DATA_KEY, "committed")?;
    store(writer)?.stage_notification_publication(&original)?;
    assert_pending(reader, None)?;
    writer.backend.commit_transaction()?;
    assert_eq!(
        reader.catalog.get_metadata(DATA_KEY)?.as_deref(),
        Some("before")
    );
    assert_pending(reader, Some(&original))?;
    reader.backend.rollback_transaction()?;
    assert_eq!(
        reader.catalog.get_metadata(DATA_KEY)?.as_deref(),
        Some("committed")
    );
    Ok(())
}

/// Verify original publication after closing every prior handle. Autonomous acknowledgement must neither commit nor roll back the caller's private SQL work; stale acknowledgements must preserve later publications.
pub fn verify_publication_reopen(session: &PersistentStorageSession) -> StorageBackendResult<()> {
    let original = publication(0)?;
    let later = publication(2)?;
    assert_eq!(
        session.catalog.get_metadata(DATA_KEY)?.as_deref(),
        Some("committed")
    );
    assert_pending(session, Some(&original))?;
    store(session)?.acknowledge_notification_publication(later.fingerprint(), &control())?;
    assert_pending(session, Some(&original))?;
    session.backend.begin_transaction()?;
    session
        .catalog
        .set_metadata(PRIVATE_KEY, "must roll back")?;
    store(session)?.acknowledge_notification_publication(original.fingerprint(), &control())?;
    assert!(session.backend.in_transaction());
    assert!(session.backend.transaction_has_written()?);
    assert_eq!(
        session.catalog.get_metadata(PRIVATE_KEY)?.as_deref(),
        Some("must roll back")
    );
    assert_pending(session, None)?;
    session.backend.rollback_transaction()?;
    assert!(session.catalog.get_metadata(PRIVATE_KEY)?.is_none());

    session.backend.begin_read_transaction()?;
    store(session)?.stage_notification_publication(&later)?;
    assert!(!session.backend.transaction_has_written()?);
    session.backend.commit_transaction()?;
    store(session)?.acknowledge_notification_publication(original.fingerprint(), &control())?;
    assert_pending(session, Some(&later))?;
    store(session)?.acknowledge_notification_publication(later.fingerprint(), &control())?;
    assert_pending(session, None)
}

/// Verify durable acknowledgement after reopening the database used by [`verify_publication_reopen`].
pub fn verify_publication_cleared(session: &PersistentStorageSession) -> StorageBackendResult<()> {
    assert_pending(session, None)?;
    assert!(session.catalog.get_metadata(PRIVATE_KEY)?.is_none());
    assert_eq!(
        session.catalog.get_metadata(DATA_KEY)?.as_deref(),
        Some("committed")
    );
    Ok(())
}
