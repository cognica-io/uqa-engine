//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual retained owners, blocked Engine gates and native registry transactions at shutdown.

use super::super::CrossProcessCoordinator;
use crate::{Engine, NotificationSubscriptionOptions, NotificationWait};
use std::{
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

fn options() -> NotificationSubscriptionOptions {
    NotificationSubscriptionOptions {
        max_active_subscriptions: 2,
        max_channels: 1,
        max_queued_notifications: 2,
        max_queued_bytes: 4_096,
        max_registry_entries_per_poll: 1,
    }
}

fn coordinator(engine: &Engine) -> Arc<CrossProcessCoordinator> {
    engine
        .notification_hub
        .cross
        .as_ref()
        .unwrap()
        .initialized_coordinator()
        .unwrap()
}

fn registry_path(path: &Path) -> PathBuf {
    let mut registry = path.as_os_str().to_owned();
    registry.push(".uqa-notification-state");
    registry.into()
}

fn until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::yield_now();
    }
    true
}

#[test]
fn final_external_owner_joins_polling_while_a_retained_handle_keeps_it_alive() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("owner.db")).unwrap();
    let subscription = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    let coordinator = coordinator(&engine);
    let recovery = coordinator.recovery_control().unwrap();
    assert!(!recovery
        .cancellation()
        .shares_signal(&engine.runtime.cancellation));
    assert!(coordinator.worker_is_retained());
    drop(engine);
    assert!(coordinator.worker_is_retained());
    assert!(!recovery.cancellation().is_cancelled());
    subscription.close();
    assert!(!coordinator.worker_is_retained());
    assert!(recovery.cancellation().is_cancelled());
    assert_eq!(Arc::strong_count(&coordinator), 1);
}

#[test]
fn concurrent_close_retains_its_completion_barrier_and_independent_listener() {
    let engine = Engine::new();
    let subscription = Arc::new(
        engine
            .subscribe_notifications(&["events"], options())
            .unwrap(),
    );
    let other = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    let gate = engine.notification_hub.commit_gate.lock();
    let first = {
        let subscription = Arc::clone(&subscription);
        std::thread::spawn(move || subscription.close())
    };
    let cleanup_entered = until(|| {
        subscription.is_closed()
            && subscription
                .resources
                .try_lock()
                .is_none_or(|resources| resources.is_none())
    });
    let completion_is_retained = subscription.resources.try_lock().is_none();
    let second = {
        let subscription = Arc::clone(&subscription);
        std::thread::spawn(move || subscription.close())
    };
    drop(gate);
    first.join().unwrap();
    second.join().unwrap();
    assert!(cleanup_entered);
    assert!(
        completion_is_retained,
        "taking resources out of the handle must not publish completed cleanup"
    );
    assert!(subscription.resources.lock().is_none());
    assert_eq!(
        subscription.wait(Duration::ZERO).unwrap(),
        NotificationWait::Closed
    );
    engine.sql("NOTIFY events, 'independent'", &[]).unwrap();
    assert!(matches!(
        other.wait(Duration::ZERO).unwrap(),
        NotificationWait::Event(_)
    ));
    let replacement = engine
        .subscribe_notifications(&["events"], options())
        .unwrap();
    assert!(!replacement.is_closed());
}

#[test]
fn stopping_delivery_wakes_receivers_but_retains_cleanup_admission() {
    let engine = Engine::new();
    let options = NotificationSubscriptionOptions {
        max_active_subscriptions: 1,
        ..options()
    };
    let subscription = Arc::new(
        engine
            .subscribe_notifications(&["events"], options)
            .unwrap(),
    );
    let gate = engine.notification_hub.commit_gate.lock();
    let (sent, received) = mpsc::channel();
    let stopping = Arc::clone(&subscription);
    let worker = std::thread::spawn(move || {
        stopping.stop_delivery();
        sent.send(()).unwrap();
    });
    let stopped = received.recv_timeout(Duration::from_secs(5));
    let retained = subscription
        .resources
        .try_lock()
        .is_some_and(|owner| owner.is_some());
    drop(gate);
    worker.join().unwrap();
    assert!(
        stopped.is_ok(),
        "delivery stop must not enter provider cleanup"
    );
    assert!(retained);
    assert_eq!(
        subscription.wait(Duration::ZERO).unwrap(),
        NotificationWait::Closed
    );
    assert_eq!(
        engine
            .subscribe_notifications(&["events"], options)
            .err()
            .unwrap()
            .kind(),
        uqa_core::notifications::NotificationFailureKind::Capacity
    );
    subscription.close();
    let replacement = engine
        .subscribe_notifications(&["events"], options)
        .unwrap();
    assert!(!replacement.is_closed());
}

#[test]
fn final_owner_cancels_the_actual_polling_worker_while_hub_state_is_held() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("held-state.db")).unwrap();
    engine.prepare_notification_recovery().unwrap();
    let owner = Arc::clone(&engine.notification_hub);
    let hub = Arc::clone(&owner.hub);
    let coordinator = coordinator(&engine);
    let recovery = coordinator.recovery_control().unwrap();
    drop(engine);
    let state = hub.state.lock();
    let (waiting, observed) = mpsc::sync_channel(1);
    coordinator.observe_worker_gate_wait(waiting);
    CrossProcessCoordinator::wake([coordinator.wake_port()]);
    let waited = observed.recv_timeout(Duration::from_secs(5));
    let (closed, completion) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        drop(owner);
        closed.send(()).unwrap();
    });
    let completed = completion.recv_timeout(Duration::from_secs(5));
    drop(state);
    worker.join().unwrap();
    assert!(
        waited.is_ok(),
        "the actual polling task must reach the held state gate"
    );
    assert!(
        completed.is_ok(),
        "shutdown must join without requiring the held gate"
    );
    assert!(recovery.cancellation().is_cancelled());
    assert!(!coordinator.worker_is_retained());
    assert!(hub.cross_error.lock().is_none());
}

#[rstest::rstest]
fn final_owner_cancels_retained_native_registry_admission(#[values(0, 1, 2, 3)] provider: usize) {
    const KEY: &str = "notification-owner-test-key";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("held-registry.db");
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
        3 => Engine::open_encrypted(&path, KEY).unwrap(),
        _ => unreachable!(),
    };
    engine.prepare_notification_recovery().unwrap();
    let coordinator = coordinator(&engine);
    let recovery = coordinator.recovery_control().unwrap();
    let registry = rusqlite::Connection::open(registry_path(&path)).unwrap();
    if provider == 3 {
        registry.pragma_update(None, "key", KEY).unwrap();
    }
    registry.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (started, attempted) = mpsc::channel();
    let (finished, result) = mpsc::channel();
    let worker = {
        let coordinator = Arc::clone(&coordinator);
        std::thread::spawn(move || {
            started.send(()).unwrap();
            let outcome = coordinator
                .begin_registry_transaction()
                .map(|_| ())
                .map_err(|error| error.sqlstate().map(str::to_owned));
            finished.send(outcome).unwrap();
        })
    };
    attempted.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(engine);
    let outcome = result.recv_timeout(Duration::from_secs(5));
    registry.execute_batch("ROLLBACK").unwrap();
    worker.join().unwrap();
    assert!(recovery.cancellation().is_cancelled());
    assert_eq!(outcome.unwrap(), Err(Some("57014".into())));
    assert!(!coordinator.worker_is_retained());
}

#[test]
fn shutdown_cancels_native_cursor_commit_without_committing_its_prepared_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("held-cursor.db");
    let engine = Engine::open(&path).unwrap();
    engine.sql("LISTEN events", &[]).unwrap();
    let coordinator = coordinator(&engine);
    let reader = rusqlite::Connection::open(registry_path(&path)).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM listeners")
        .unwrap();
    let probe = rusqlite::Connection::open(registry_path(&path)).unwrap();
    probe.busy_timeout(Duration::ZERO).unwrap();
    let (finished, result) = mpsc::channel();
    let worker = {
        let hub = Arc::clone(&engine.notification_hub.hub);
        let session_id = engine.session_id;
        std::thread::spawn(move || {
            let outcome = hub
                .try_synchronize_cross_process_session(Some((session_id, true)))
                .map_err(|error| error.sqlstate().map(str::to_owned));
            finished.send(outcome).unwrap();
        })
    };
    let native_commit_waited = until(|| {
        probe
            .query_row("SELECT count(*) FROM listeners", [], |row| {
                row.get::<_, i64>(0)
            })
            .is_err_and(|error| {
                error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
            })
    });
    let (stopped, completion) = mpsc::channel();
    let stopping = std::thread::spawn(move || {
        coordinator.shutdown();
        stopped.send(()).unwrap();
    });
    let outcome = result.recv_timeout(Duration::from_secs(5));
    let stop_completed = completion.recv_timeout(Duration::from_secs(5));
    reader.execute_batch("ROLLBACK").unwrap();
    worker.join().unwrap();
    stopping.join().unwrap();
    assert!(
        native_commit_waited,
        "the pending native writer must exclude new readers"
    );
    assert_eq!(outcome.unwrap(), Err(Some("57014".into())));
    assert!(stop_completed.is_ok());
    assert!(!engine.runtime.cancellation.is_cancelled());
    assert!(!reader
        .query_row("SELECT transaction_open FROM listeners", [], |row| row
            .get::<_, bool>(0))
        .unwrap());
    assert!(!engine.notification_hub.state.lock().listeners[&engine.session_id].transaction_open);
}
