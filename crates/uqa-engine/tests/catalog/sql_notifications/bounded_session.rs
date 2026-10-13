//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded LISTEN preserves its original SQL session and transaction histories.

use super::{exec, values};
use uqa_engine::{Engine, SQLNotificationOptions};

fn options() -> SQLNotificationOptions {
    SQLNotificationOptions {
        max_channels: 2,
        max_queued_notifications: 4,
        max_queued_bytes: 65_536,
        max_registry_entries_per_poll: 2,
    }
}

fn transaction_history(root: &Engine) {
    let listener = root.new_session().unwrap();
    let sender = root.new_session().unwrap();
    listener.configure_sql_notifications(options()).unwrap();
    let pid = listener.backend_process_id();
    exec(&listener, "BEGIN; LISTEN absent; ROLLBACK; LISTEN events");
    assert_eq!(listener.backend_process_id(), pid);
    exec(&listener, "BEGIN");
    exec(&sender, "NOTIFY absent, 'skip'; BEGIN; NOTIFY events, 'one'; SAVEPOINT nested; NOTIFY events, 'discard'; ROLLBACK TO nested; NOTIFY events, 'two'; COMMIT");
    assert!(listener.try_take_sql_notifications().unwrap().is_empty());
    exec(&listener, "ROLLBACK");
    let mut observed = Vec::new();
    // Polling is bounded, and completion is the exact two accepted events, not elapsed time.
    while observed.len() != 2 {
        observed.extend(listener.try_take_sql_notifications().unwrap());
        assert!(observed.len() <= 2);
    }
    assert!(observed
        .iter()
        .all(|n| n.process_id == sender.backend_process_id()));
    assert_eq!(
        values(observed),
        [
            ("events".into(), "one".into()),
            ("events".into(), "two".into())
        ]
    );
    exec(&listener, "BEGIN; UNLISTEN events; ROLLBACK");
    exec(&sender, "NOTIFY events, 'retained'");
    assert_eq!(
        values(listener.try_take_sql_notifications().unwrap()),
        [("events".into(), "retained".into())]
    );
    exec(&listener, "UNLISTEN *");
    exec(&sender, "NOTIFY events, 'removed'");
    assert!(listener.try_take_sql_notifications().unwrap().is_empty());
    listener.close().unwrap();
}

#[test]
fn bounded_memory_sql_listener_preserves_transactions_and_sender_identity() {
    let listener = Engine::new();
    listener.configure_sql_notifications(options()).unwrap();
    let pid = listener.backend_process_id();
    exec(&listener, "BEGIN; LISTEN absent; ROLLBACK; LISTEN events; BEGIN; NOTIFY events, 'one'; SAVEPOINT nested; NOTIFY events, 'discard'; ROLLBACK TO nested; NOTIFY events, 'two'");
    assert!(listener.try_take_sql_notifications().unwrap().is_empty());
    exec(&listener, "COMMIT");
    let observed = listener.try_take_sql_notifications().unwrap();
    assert!(observed.iter().all(|n| n.process_id == pid));
    assert_eq!(
        values(observed),
        [
            ("events".into(), "one".into()),
            ("events".into(), "two".into())
        ]
    );
}

#[test]
fn bounded_native_sql_listener_preserves_transactions_and_sender_identity() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("bounded.db")).unwrap();
    transaction_history(&root);
}

#[test]
fn bounded_session_overflow_is_terminal_and_never_reports_missing_events_as_success() {
    for native in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = if native {
            Engine::open(&directory.path().join("overflow.db")).unwrap()
        } else {
            Engine::new()
        };
        let native_listener = native.then(|| root.new_session().unwrap());
        let listener = native_listener.as_ref().unwrap_or(&root);
        let mut bounds = options();
        bounds.max_queued_notifications = 1;
        listener.configure_sql_notifications(bounds).unwrap();
        exec(listener, "LISTEN events");
        exec(&root, "NOTIFY events, 'one'; NOTIFY events, 'two'");
        for _ in 0..2 {
            let failure = listener.try_take_sql_notifications().unwrap_err();
            assert!(failure.to_string().contains("NOTIFICATION_BACKPRESSURE"));
        }
        listener.close().unwrap();
        exec(&root, "NOTIFY events, 'after close'");
    }
}

#[test]
fn byte_admission_is_terminal_before_an_oversized_payload_copy() {
    let engine = Engine::new();
    let mut bounds = options();
    bounds.max_queued_bytes = 1;
    engine.configure_sql_notifications(bounds).unwrap();
    exec(&engine, "LISTEN events; NOTIFY events, 'payload'");
    assert!(engine
        .try_take_sql_notifications()
        .unwrap_err()
        .to_string()
        .contains("NOTIFICATION_BACKPRESSURE"));
}

#[test]
fn configuration_is_once_before_listening_and_channel_limit_rolls_back() {
    let engine = Engine::new();
    let mut bounds = options();
    bounds.max_channels = 0;
    assert!(engine.configure_sql_notifications(bounds).is_err());
    bounds.max_channels = 1;
    engine.configure_sql_notifications(bounds).unwrap();
    assert!(engine.configure_sql_notifications(bounds).is_err());
    exec(&engine, "LISTEN retained");
    let error = engine.sql("LISTEN excess", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("54000"));
    exec(&engine, "NOTIFY retained, 'one'; NOTIFY excess, 'skip'");
    assert_eq!(
        values(engine.try_take_sql_notifications().unwrap()),
        [("retained".into(), "one".into())]
    );
    let active = Engine::new();
    exec(&active, "BEGIN");
    assert!(active.configure_sql_notifications(bounds).is_err());
    exec(&active, "ROLLBACK; LISTEN existing");
    assert!(active.configure_sql_notifications(bounds).is_err());
}
