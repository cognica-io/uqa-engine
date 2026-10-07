//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent subscriptions through actual memory and persistent provider instances.

#[cfg(unix)]
#[path = "subscriptions/source_identity.rs"]
mod source_identity;

use super::{exec, values};
use std::{sync::Arc, task::Poll, time::Duration};
use uqa_core::notifications::{NotificationEvent, NotificationFailureKind};
use uqa_engine::{
    Engine, NotificationSubscription, NotificationSubscriptionOptions, NotificationWait,
};

fn options() -> NotificationSubscriptionOptions {
    NotificationSubscriptionOptions {
        max_active_subscriptions: 8,
        max_channels: 4,
        max_queued_notifications: 64,
        max_queued_bytes: 65_536,
        max_registry_entries_per_poll: 2,
    }
}

fn take(
    subscription: &NotificationSubscription,
    sequence: u64,
    channel: &str,
    payload: &str,
    pid: i32,
) {
    let NotificationWait::Event(NotificationEvent::Notification {
        identity,
        sequence: actual,
        notification,
    }) = subscription.wait(Duration::from_secs(5)).unwrap()
    else {
        panic!("committed notification expected")
    };
    assert_eq!(&identity, subscription.identity());
    assert!(identity.request_id.is_none());
    assert_eq!(actual, sequence);
    assert_eq!(
        (
            notification.channel.as_str(),
            notification.payload.as_str(),
            notification.process_id
        ),
        (channel, payload, pid)
    );
}

#[test]
fn memory_subscriptions_have_independent_boundaries_and_do_not_drain_sql_listener() {
    let engine = Engine::new();
    exec(&engine, "LISTEN alpha; NOTIFY alpha, 'before'");
    let first = engine
        .subscribe_notifications(&["alpha", "beta"], options())
        .unwrap();
    exec(&engine, "BEGIN; NOTIFY alpha, 'one'; NOTIFY alpha, 'one'; SAVEPOINT nested; NOTIFY beta, 'discarded'; ROLLBACK TO nested");
    let second = engine
        .subscribe_notifications(&["alpha"], options())
        .unwrap();
    assert_ne!(first.identity().epoch, second.identity().epoch);
    assert!(matches!(first.poll().unwrap(), Poll::Pending));
    assert!(matches!(second.poll().unwrap(), Poll::Pending));
    exec(&engine, "NOTIFY beta, 'two'; COMMIT");
    take(&first, 1, "alpha", "one", engine.backend_process_id());
    take(&first, 2, "beta", "two", engine.backend_process_id());
    take(&second, 1, "alpha", "one", engine.backend_process_id());
    assert_eq!(
        values(engine.take_sql_notifications()),
        [
            ("alpha".into(), "before".into()),
            ("alpha".into(), "one".into())
        ]
    );
    first.close();
    first.close();
    exec(&engine, "NOTIFY alpha, 'after close'");
    take(
        &second,
        2,
        "alpha",
        "after close",
        engine.backend_process_id(),
    );
    assert_eq!(first.poll().unwrap(), Poll::Ready(None));
    let unrelated = Engine::new();
    exec(&unrelated, "NOTIFY alpha, 'other memory database'");
    assert!(matches!(second.poll().unwrap(), Poll::Pending));
}

#[test]
fn exact_channel_validation_and_capacity_do_not_partially_register() {
    let engine = Engine::new();
    for channels in [
        &[][..],
        &[""][..],
        &["alpha", "alpha"][..],
        &["nul\0channel"][..],
        &["éééééééééééééééééééééééééééééééé"][..],
    ] {
        assert_eq!(
            engine
                .subscribe_notifications(channels, options())
                .err()
                .unwrap()
                .kind(),
            NotificationFailureKind::InvalidRequest
        );
    }
    let single = engine
        .subscribe_notifications(
            &["MixedCase"],
            NotificationSubscriptionOptions {
                max_active_subscriptions: 1,
                ..options()
            },
        )
        .unwrap();
    assert_eq!(
        engine
            .subscribe_notifications(&["other"], options())
            .err()
            .unwrap()
            .kind(),
        NotificationFailureKind::Capacity
    );
    exec(
        &engine,
        "NOTIFY mixedcase, 'ignored'; NOTIFY \"MixedCase\", 'exact'",
    );
    take(
        &single,
        1,
        "MixedCase",
        "exact",
        engine.backend_process_id(),
    );
    assert!(matches!(single.poll().unwrap(), Poll::Pending));
    single.close();
    assert!(engine
        .subscribe_notifications(&["other"], options())
        .is_ok());
}

#[test]
fn selected_nologin_role_is_retained_without_creating_a_new_sql_session() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE ROLE notification_reader NOLOGIN; SET ROLE notification_reader",
    );
    let subscription = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    let selected = exec(
        &engine,
        "SELECT oid FROM pg_roles WHERE rolname = current_user",
    );
    assert_eq!(
        selected.rows[0].get("oid"),
        Some(&uqa_core::Value::Int(subscription.role_identity().oid))
    );
    exec(&engine, "RESET ROLE; NOTIFY events, 'retained'");
    take(
        &subscription,
        1,
        "events",
        "retained",
        engine.backend_process_id(),
    );
    let privileged = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    assert_ne!(subscription.role_identity(), privileged.role_identity());
}

#[test]
fn slow_subscriber_overflow_is_visible_and_does_not_block_healthy_or_sql_queues() {
    let engine = Engine::new();
    exec(&engine, "LISTEN events");
    let slow = engine
        .subscribe_notifications(
            &["events"],
            NotificationSubscriptionOptions {
                max_queued_notifications: 1,
                ..options()
            },
        )
        .unwrap();
    let healthy = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    exec(&engine, "NOTIFY events, 'first'; NOTIFY events, 'second'");
    assert_eq!(
        slow.poll().unwrap_err().kind(),
        NotificationFailureKind::Backpressure
    );
    take(&healthy, 1, "events", "first", engine.backend_process_id());
    take(&healthy, 2, "events", "second", engine.backend_process_id());
    assert_eq!(engine.take_sql_notifications().len(), 2);
    exec(&engine, "SELECT 1");
}

#[test]
fn failed_cursor_commit_closes_each_receiver_without_exposing_private_delivery() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failed-delivery.db");
    let engine = Engine::open(&path).unwrap();
    let first = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    let second = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    let registry = rusqlite::Connection::open(super::encryption::registry_path(&path)).unwrap();
    registry.execute_batch("CREATE TABLE required_cursor(id INTEGER PRIMARY KEY); CREATE TABLE rejected_cursor(id INTEGER REFERENCES required_cursor(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_cursor_commit AFTER UPDATE OF next_sequence ON listeners WHEN NEW.next_sequence > OLD.next_sequence BEGIN INSERT INTO rejected_cursor VALUES(1); END;").unwrap();
    let error = engine
        .sql("NOTIFY events, 'private delivery'", &[])
        .unwrap_err();
    assert!(
        error.to_string().contains("transaction committed"),
        "{error}"
    );
    for subscriber in [&first, &second] {
        let error = subscriber.wait(Duration::from_secs(5)).unwrap_err();
        assert_eq!(error.kind(), NotificationFailureKind::SourceUnavailable);
        assert!(error.original_error().unwrap().is::<uqa_sql::SQLError>());
        assert!(!format!("{error:?} {error}").contains("private delivery"));
        assert_eq!(
            subscriber.poll().unwrap_err().kind(),
            NotificationFailureKind::SourceUnavailable
        );
    }
    registry
        .execute_batch("DROP TRIGGER reject_cursor_commit")
        .unwrap();
    let replacement = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    assert!(matches!(replacement.poll().unwrap(), Poll::Pending));
    exec(&engine, "NOTIFY events, 'new boundary'");
    take(
        &replacement,
        1,
        "events",
        "new boundary",
        engine.backend_process_id(),
    );
}

#[rstest::rstest]
fn persistent_listener_survives_caller_and_scans_every_bounded_prefix(
    #[values(0, 1, 2, 3, 4, 5, 6)] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("owned.db");
    let key = super::encryption::KEY;
    let engine = match provider {
        0 => Engine::open(&path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::open_encrypted(&path, key).unwrap(),
        4 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::from_connection(
                uqa_storage_sqlite::ManagedConnection::open_encrypted(&path, key).unwrap(),
            )
            .unwrap(),
        ))
        .unwrap(),
        5 => Engine::open_compressed(
            &path,
            uqa_storage_sqlite::SQLiteCompressionOptions::default(),
        )
        .unwrap(),
        6 => Engine::open_compressed_encrypted(
            &path,
            key,
            uqa_storage_sqlite::SQLiteCompressionOptions::default(),
        )
        .unwrap(),
        _ => unreachable!(),
    };
    let sender = engine.new_session().unwrap();
    exec(&engine, "LISTEN events; BEGIN");
    let subscription = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    exec(&sender, "BEGIN; NOTIFY events, 'first'; NOTIFY ignored, 'not selected'; NOTIFY events, 'second'; NOTIFY ignored, 'also ignored'; NOTIFY events, 'third'; COMMIT");
    take(
        &subscription,
        1,
        "events",
        "first",
        sender.backend_process_id(),
    );
    assert_eq!(engine.take_sql_notifications().len(), 0);
    drop(engine);
    take(
        &subscription,
        2,
        "events",
        "second",
        sender.backend_process_id(),
    );
    take(
        &subscription,
        3,
        "events",
        "third",
        sender.backend_process_id(),
    );
    exec(
        &sender,
        "BEGIN; NOTIFY events, 'discarded'; ROLLBACK; NOTIFY events, 'after drop'",
    );
    take(
        &subscription,
        4,
        "events",
        "after drop",
        sender.backend_process_id(),
    );
    if matches!(provider, 3 | 4 | 6) {
        super::encryption::assert_no_plaintext(&path, &["after drop", key]);
    }
    subscription.close();
    assert_eq!(
        subscription.wait(Duration::ZERO).unwrap(),
        NotificationWait::Closed
    );
    exec(&sender, "NOTIFY events, 'closed'");
}
