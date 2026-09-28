//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Global coordination must not decode another owner's channel list.

use super::*;
use crate::{Engine, NotificationSubscriptionOptions, NotificationWait};
use std::time::Duration;

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
    let mut registry_path = path.as_os_str().to_owned();
    registry_path.push(".uqa-notification-state");
    {
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
    assert!(legacy.take_sql_notifications().is_empty());
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
    assert!(registry.entries_from(0).unwrap().is_empty());
    registry.commit().unwrap();
}
