//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public registration cancellation at actual Engine gates and native commit admission.

use super::{Arc, CancellationToken, NotificationFailureKind};
use crate::{Engine, NotificationSubscriptionOptions};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

fn options() -> NotificationSubscriptionOptions {
    NotificationSubscriptionOptions {
        max_active_subscriptions: 1,
        max_channels: 1,
        max_queued_notifications: 2,
        max_queued_bytes: 4_096,
        max_registry_entries_per_poll: 1,
    }
}

fn until(mut observed: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !observed() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::yield_now();
    }
    true
}

fn pending(engine: &Engine) -> usize {
    engine
        .notification_hub
        .admissions
        .retained
        .lock()
        .iter()
        .filter(|permit| permit.strong_count() != 0)
        .count()
}

#[test]
fn pre_submission_permit_shares_the_ready_limit_without_waiting_on_engine_gates() {
    let engine = Engine::new();
    let token = CancellationToken::new();
    let statement = engine.runtime.statement_gate.lock();
    let hub = engine.notification_hub.state.lock();
    let permit = engine
        .reserve_notification_subscription(options(), &token)
        .unwrap();
    assert_eq!(pending(&engine), 1);
    let looser = NotificationSubscriptionOptions {
        max_active_subscriptions: 64,
        ..options()
    };
    assert_eq!(
        engine
            .reserve_notification_subscription(looser, &token)
            .unwrap_err()
            .kind(),
        NotificationFailureKind::Capacity,
    );
    drop(hub);
    drop(statement);
    let subscription = engine
        .subscribe_notifications_with_permit(&["events"], permit, &token)
        .unwrap();
    assert_eq!(pending(&engine), 1);
    subscription.stop_delivery();
    assert!(engine
        .reserve_notification_subscription(looser, &token)
        .is_err());
    subscription.close();
    assert_eq!(pending(&engine), 0);
    drop(
        engine
            .reserve_notification_subscription(options(), &token)
            .unwrap(),
    );
    assert_eq!(pending(&engine), 0);
}

#[test]
fn unused_permit_release_does_not_retain_or_rebind_the_original_hub() {
    let original = Engine::new();
    let token = CancellationToken::new();
    let permit = original
        .reserve_notification_subscription(options(), &token)
        .unwrap();
    let original_hub = Arc::downgrade(&original.notification_hub);
    drop(original);
    assert!(original_hub.upgrade().is_none());
    let other = Engine::new();
    assert_eq!(
        other
            .subscribe_notifications_with_permit(&["events"], permit, &token)
            .err()
            .unwrap()
            .kind(),
        NotificationFailureKind::InvalidRequest,
    );
    assert_eq!(pending(&other), 0);
    other
        .subscribe_notifications(&["events"], options())
        .unwrap()
        .close();
}

#[test]
fn invalid_or_cancelled_reserved_registration_releases_its_original_capacity() {
    let engine = Engine::new();
    let options = NotificationSubscriptionOptions {
        max_channels: 3,
        ..options()
    };
    let token = CancellationToken::new();
    for channels in [&["events", "other", "events"][..], &["a\0b"], &[""], &[]] {
        let permit = engine
            .reserve_notification_subscription(options, &token)
            .unwrap();
        assert_eq!(
            engine
                .subscribe_notifications_with_permit(channels, permit, &token)
                .err()
                .unwrap()
                .kind(),
            NotificationFailureKind::InvalidRequest,
        );
        assert_eq!(pending(&engine), 0);
    }
    let permit = engine
        .reserve_notification_subscription(options, &token)
        .unwrap();
    token.cancel();
    assert_eq!(
        engine
            .subscribe_notifications_with_permit(&["invalid\0"], permit, &token)
            .err()
            .unwrap()
            .kind(),
        NotificationFailureKind::Cancelled,
    );
    assert_eq!(pending(&engine), 0);
    assert_eq!(
        engine
            .reserve_notification_subscription(options, &token)
            .unwrap_err()
            .kind(),
        NotificationFailureKind::Cancelled,
    );
    assert_eq!(pending(&engine), 0);
}

fn blocked_registration<T>(engine: &Arc<Engine>, held: T) {
    let cancellation = CancellationToken::new();
    let (send, receive) = mpsc::channel();
    let (waiting, observed) = mpsc::sync_channel(1);
    let worker = {
        let engine = Arc::clone(engine);
        let cancellation = cancellation.clone();
        std::thread::spawn(move || {
            super::observe_gate_wait(waiting);
            let result = engine
                .subscribe_notifications_with_cancellation(&["events"], options(), &cancellation)
                .map(|subscription| subscription.close())
                .map_err(|error| error.kind());
            send.send(result).unwrap();
        })
    };
    let admitted = observed.recv_timeout(Duration::from_secs(5)).is_ok();
    cancellation.cancel();
    let result = receive.recv_timeout(Duration::from_secs(5));
    drop(held);
    worker.join().unwrap();
    assert!(
        admitted,
        "the request must own admission before cancellation"
    );
    assert_eq!(result.unwrap(), Err(NotificationFailureKind::Cancelled));
    assert_eq!(pending(engine), 0);
    assert!(engine.notification_hub.state.lock().listeners.is_empty());
    engine
        .subscribe_notifications(&["events"], options())
        .unwrap()
        .close();
}

#[test]
fn cancellation_during_statement_wait_releases_pending_capacity_without_unregistering() {
    let engine = Arc::new(Engine::new());
    let held = engine.runtime.statement_gate.lock();
    blocked_registration(&engine, held);
}

#[test]
fn cancellation_during_hub_commit_wait_does_not_wait_again_during_drop() {
    let engine = Arc::new(Engine::new());
    let held = engine.notification_hub.commit_gate.lock();
    blocked_registration(&engine, held);
}

#[test]
fn cancellation_during_hub_state_wait_leaves_no_listener() {
    let engine = Arc::new(Engine::new());
    let held = engine.notification_hub.state.lock();
    blocked_registration(&engine, held);
}

#[test]
fn cancellation_during_authorization_wait_retains_no_default_role_listener() {
    let engine = Arc::new(Engine::new());
    let held = engine.session.state.write();
    blocked_registration(&engine, held);
}

#[test]
fn pending_registration_applies_its_ceiling_before_another_request_can_wait() {
    let engine = Arc::new(Engine::new());
    let held = engine.runtime.statement_gate.lock();
    let cancellation = CancellationToken::new();
    let (waiting, observed) = mpsc::sync_channel(1);
    let worker = {
        let engine = Arc::clone(&engine);
        let cancellation = cancellation.clone();
        std::thread::spawn(move || {
            super::observe_gate_wait(waiting);
            engine
                .subscribe_notifications_with_cancellation(&["events"], options(), &cancellation)
                .map(|subscription| subscription.close())
                .map_err(|error| error.kind())
        })
    };
    let admitted = observed.recv_timeout(Duration::from_secs(5)).is_ok();
    let other = engine
        .subscribe_notifications(
            &["other"],
            NotificationSubscriptionOptions {
                max_active_subscriptions: 64,
                ..options()
            },
        )
        .err()
        .map(|error| error.kind());
    cancellation.cancel();
    drop(held);
    let result = worker.join().unwrap();
    assert!(admitted);
    assert_eq!(other, Some(NotificationFailureKind::Capacity));
    assert_eq!(result, Err(NotificationFailureKind::Cancelled));
    assert_eq!(pending(&engine), 0);
    let replacement = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    assert_eq!(pending(&engine), 1);
    replacement.close();
    assert_eq!(pending(&engine), 0);
}

#[test]
fn registration_signal_is_independent_of_sql_cancellation_and_ready_handle_lifetime() {
    let engine = Engine::new();
    engine.runtime.cancellation.cancel();
    let registration = CancellationToken::new();
    let subscription = engine
        .subscribe_notifications_with_cancellation(&["events"], options(), &registration)
        .unwrap();
    assert!(engine.runtime.cancellation.is_cancelled());
    registration.cancel();
    assert!(!subscription.is_closed());
    engine.runtime.cancellation.reset();
    engine.sql("NOTIFY events, 'ready wins'", &[]).unwrap();
    assert!(matches!(
        subscription.wait(Duration::from_secs(1)).unwrap(),
        crate::NotificationWait::Event(_)
    ));
    subscription.close();
    assert_eq!(pending(&engine), 0);
    assert_eq!(
        engine
            .subscribe_notifications_with_cancellation(&["events"], options(), &registration)
            .err()
            .unwrap()
            .kind(),
        NotificationFailureKind::Cancelled
    );
    assert_eq!(pending(&engine), 0);
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
#[test]
fn cancellation_during_coordinator_initialization_wait_preserves_the_original_registry() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Arc::new(Engine::open(&directory.path().join("coordinator.db")).unwrap());
    let held = engine
        .notification_hub
        .cross
        .as_ref()
        .unwrap()
        .coordinator
        .lock();
    blocked_registration(&engine, held);
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
#[test]
fn cancellation_during_registry_initialization_wait_preserves_the_original_pool() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Arc::new(Engine::open(&directory.path().join("registry.db")).unwrap());
    let held = engine
        .notification_hub
        .cross
        .as_ref()
        .unwrap()
        .registry
        .lock();
    blocked_registration(&engine, held);
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
#[test]
fn cancellation_after_native_writer_admission_rolls_back_registration_and_lease() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("commit.db");
    let engine = Arc::new(Engine::open(&path).unwrap());
    engine
        .subscribe_notifications(&["events"], options())
        .unwrap()
        .close();
    let mut registry_path = path.as_os_str().to_owned();
    registry_path.push(".uqa-notification-state");
    let reader = rusqlite::Connection::open(std::path::PathBuf::from(&registry_path)).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM listeners")
        .unwrap();
    let probe = rusqlite::Connection::open(std::path::PathBuf::from(&registry_path)).unwrap();
    probe.busy_timeout(Duration::ZERO).unwrap();
    let cancellation = CancellationToken::new();
    let (send, receive) = mpsc::channel();
    let worker = {
        let engine = Arc::clone(&engine);
        let cancellation = cancellation.clone();
        std::thread::spawn(move || {
            let result = engine
                .subscribe_notifications_with_cancellation(&["events"], options(), &cancellation)
                .map(|subscription| subscription.close())
                .map_err(|error| error.kind());
            send.send(result).unwrap();
        })
    };
    let writer_observed = until(|| match probe.execute_batch("BEGIN IMMEDIATE") {
        Ok(()) => {
            probe.execute_batch("ROLLBACK").unwrap();
            false
        }
        Err(error) => error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy),
    });
    let lease_observed = until(|| {
        std::fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".lease")
        })
    });
    cancellation.cancel();
    let result = receive.recv_timeout(Duration::from_secs(5));
    reader.execute_batch("ROLLBACK").unwrap();
    worker.join().unwrap();
    assert!(
        writer_observed,
        "the actual registry writer must be admitted"
    );
    assert!(
        lease_observed,
        "registration must own its actual listener lease before cancellation"
    );
    assert_eq!(result.unwrap(), Err(NotificationFailureKind::Cancelled));
    assert_eq!(pending(&engine), 0);
    assert_eq!(
        reader
            .query_row("SELECT count(*) FROM listeners", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(std::fs::read_dir(directory.path())
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".lease")));
    engine
        .subscribe_notifications(&["events"], options())
        .unwrap()
        .close();
}
