//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Global coordination must not decode another owner's channel list.

use super::*;
use crate::{Engine, NotificationSubscriptionOptions, NotificationWait};
use std::time::Duration;

fn corrupt_foreign_channel_metadata(path: &std::path::Path) {
    let mut registry_path = path.as_os_str().to_owned();
    registry_path.push(".uqa-notification-state");
    let fixture = rusqlite::Connection::open(std::path::PathBuf::from(registry_path)).unwrap();
    let version: i64 = fixture
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    fixture
        .create_scalar_function(
            "__uqa_notification_writer_format",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            move |_| Ok(version),
        )
        .unwrap();
    fixture
        .execute("UPDATE listeners SET channels_json = '['", [])
        .unwrap();
}

#[test]
fn foreign_channels_do_not_enter_registration_delivery_usage_or_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign-listener.db");
    let engine = Engine::open(&path).unwrap();
    engine.prepare_notification_recovery().unwrap();
    let cross = engine
        .notification_hub
        .cross
        .as_ref()
        .unwrap()
        .coordinator()
        .unwrap();
    let foreign = cross.create_listener_lease().unwrap();
    let registry = cross.begin_registry_transaction().unwrap();
    registry
        .save_listener(&CrossProcessListenerRow {
            owner_id: foreign.owner_id(),
            session_id: u64::MAX,
            process_id: 42,
            wake_port: cross.wake_port(),
            channels: vec!["foreign".into()],
            transaction_open: true,
            next_sequence: 0,
            position: 0,
        })
        .unwrap();
    registry.commit().unwrap();
    corrupt_foreign_channel_metadata(&path);
    let options = NotificationSubscriptionOptions {
        max_active_subscriptions: 1,
        max_channels: 1,
        max_queued_notifications: 2,
        max_queued_bytes: 4_096,
        max_registry_entries_per_poll: 1,
    };
    let subscription = engine
        .subscribe_notifications(&["events"], options)
        .unwrap();
    let legacy = engine.new_session().unwrap();
    legacy.sql("LISTEN events", &[]).unwrap();
    legacy.sql("BEGIN", &[]).unwrap();
    engine.sql("NOTIFY events, 'unchanged'", &[]).unwrap();
    let NotificationWait::Event(uqa_core::notifications::NotificationEvent::Notification {
        notification,
        sequence,
        ..
    }) = subscription.wait(Duration::from_secs(5)).unwrap()
    else {
        panic!("committed notification must reach its owning listener");
    };
    assert_eq!(sequence, 1);
    assert_eq!(notification.channel, "events");
    assert_eq!(notification.payload, "unchanged");
    assert_eq!(notification.process_id, engine.backend_process_id());
    legacy.poll_sql_notifications().unwrap();
    assert_eq!(legacy.take_sql_notifications().len(), 0);
    legacy.commit().unwrap();
    assert_eq!(legacy.take_sql_notifications(), [notification]);
    legacy.sql("UNLISTEN *", &[]).unwrap();
    assert_eq!(engine.notification_hub.usage().unwrap(), 0.0);
    let registry = cross.begin_registry_transaction().unwrap();
    assert_eq!(
        registry.entries_from(0).unwrap().len(),
        1,
        "foreign cursor retains the original entry"
    );
    registry.commit().unwrap();
    subscription.close();
    // Retirement releases the native lease; a registry pass reaps its dead row.
    // The pass must still avoid decoding the unrelated live owner's channels.
    assert_eq!(engine.notification_hub.usage().unwrap(), 0.0);
    let registry = cross.begin_registry_transaction().unwrap();
    let remaining = registry.listener_metadata_after(None).unwrap().unwrap();
    assert_eq!(remaining.key.owner_id, foreign.owner_id());
    assert!(registry
        .listener_metadata_after(Some(remaining.key))
        .unwrap()
        .is_none());
    registry.commit().unwrap();
    drop(foreign);
    assert_eq!(engine.notification_hub.usage().unwrap(), 0.0);
    let registry = cross.begin_registry_transaction().unwrap();
    assert!(registry.listener_metadata_after(None).unwrap().is_none());
    assert_eq!(registry.entries_from(0).unwrap().len(), 0);
    registry.commit().unwrap();
}

#[test]
fn delivery_failures_after_a_publication_are_its_delivery_failures() {
    let mut state = NotificationHubState::default();
    let published = state.delivery_failures;
    assert!(NotificationHub::concurrent_delivery_failure(&state, Some(published)).is_ok());
    let failure = NotificationHub::complete_deliveries(
        Err(SQLError::Internal("listener cursor commit failed".into())),
        &mut state,
        Vec::new(),
    )
    .unwrap_err();
    let reported = NotificationHub::concurrent_delivery_failure(&state, Some(published))
        .unwrap_err()
        .to_string();
    assert_eq!(reported, failure.to_string());
    assert!(
        NotificationHub::concurrent_delivery_failure(&state, Some(state.delivery_failures)).is_ok(),
        "a failure that precedes the publication is not its delivery failure"
    );
    assert!(NotificationHub::concurrent_delivery_failure(&state, None).is_ok());
}

#[test]
fn a_poll_that_fails_delivery_after_publication_fails_the_committing_notify() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raced-delivery.db");
    let engine = Engine::open(&path).unwrap();
    let options = NotificationSubscriptionOptions {
        max_active_subscriptions: 1,
        max_channels: 1,
        max_queued_notifications: 2,
        max_queued_bytes: 4_096,
        max_registry_entries_per_poll: 1,
    };
    let subscription = engine
        .subscribe_notifications(&["events"], options)
        .unwrap();
    let mut registry_path = path.as_os_str().to_owned();
    registry_path.push(".uqa-notification-state");
    let registry = rusqlite::Connection::open(std::path::PathBuf::from(registry_path)).unwrap();
    registry.execute_batch("CREATE TABLE required_cursor(id INTEGER PRIMARY KEY); CREATE TABLE rejected_cursor(id INTEGER REFERENCES required_cursor(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_cursor_commit AFTER UPDATE OF next_sequence ON listeners WHEN NEW.next_sequence > OLD.next_sequence BEGIN INSERT INTO rejected_cursor VALUES(1); END;").unwrap();
    let (polled, poll) = std::sync::mpsc::channel();
    *engine.notification_hub.after_publication.lock() = Some(Box::new(move |hub| {
        // Whichever of this synchronization and the background poll the publication woke runs first fails the delivery before the committing session synchronizes.
        let interleaved = hub.try_synchronize_cross_process_session(None);
        polled
            .send((interleaved.is_err(), hub.state.lock().delivery_failures))
            .unwrap();
    }));
    let error = engine.sql("NOTIFY events, 'raced'", &[]).unwrap_err();
    let (interleaved_failed, failures) = poll.try_recv().unwrap();
    assert_ne!(failures, 0, "interleaved failure: {interleaved_failed}");
    assert!(
        error
            .to_string()
            .contains("transaction committed; notification delivery awaits recovery"),
        "{error}"
    );
    assert!(!error.to_string().contains("raced"), "{error}");
    assert_eq!(
        subscription.poll().unwrap_err().kind(),
        uqa_core::notifications::NotificationFailureKind::SourceUnavailable
    );
}
