//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native cross-process notification delivery and recovery coverage.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

use super::{exec, values};

fn wait_for_notification_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "notification handshake file {} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_notification_value(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match std::fs::read_to_string(path) {
            Ok(contents) if !contents.is_empty() => return contents,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!(
                "failed to read notification handshake file {}: {error}",
                path.display()
            ),
        }
        assert!(
            Instant::now() < deadline,
            "notification handshake file {} never received a value",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_notification_result(path: &Path) -> (i32, String, String) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let mut lines = contents.lines();
                if let (Some(process_id), Some(channel), Some(payload), None) =
                    (lines.next(), lines.next(), lines.next(), lines.next())
                {
                    return (
                        process_id.parse().unwrap(),
                        channel.to_string(),
                        payload.to_string(),
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!(
                "failed to read notification result {}: {error}",
                path.display()
            ),
        }
        assert!(
            Instant::now() < deadline,
            "notification result {} was not completed",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn spawn_notification_child(database: &Path, handshake: &Path, mode: &str) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("engine_catalog::sql_notifications::cross_process::cross_process_notification_listener_child")
        .arg("--nocapture")
        .env("UQA_NOTIFICATION_CHILD_DB", database)
        .env("UQA_NOTIFICATION_CHILD_DIR", handshake)
        .env("UQA_NOTIFICATION_CHILD_MODE", mode)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

fn notification_registry_path(database: &Path) -> std::path::PathBuf {
    let mut path = database.as_os_str().to_owned();
    path.push(".uqa-notification-state");
    path.into()
}

/// Child half of the cross-process notification tests. It runs only when a parent test re-executes this test descriptor with handshake variables.
#[test]
fn cross_process_notification_listener_child() {
    let Ok(database) = std::env::var("UQA_NOTIFICATION_CHILD_DB") else {
        return;
    };
    let handshake = std::path::PathBuf::from(std::env::var("UQA_NOTIFICATION_CHILD_DIR").unwrap());
    let mode = std::env::var("UQA_NOTIFICATION_CHILD_MODE").unwrap();
    let engine = Engine::open(Path::new(&database)).unwrap();
    exec(&engine, "LISTEN cross_process_events");
    if mode == "transaction" {
        exec(&engine, "BEGIN");
    }
    std::fs::write(
        handshake.join(format!("{mode}-ready")),
        engine.backend_process_id().to_string(),
    )
    .unwrap();
    if mode == "crash" {
        std::process::exit(0);
    }
    if mode == "transaction" {
        assert!(!engine
            .wait_for_sql_notifications(Duration::from_millis(500))
            .unwrap());
        std::fs::write(handshake.join("transaction-wait-finished"), b"1").unwrap();
        wait_for_notification_file(&handshake.join("release-transaction"));
        exec(&engine, "COMMIT");
    }
    assert!(engine
        .wait_for_sql_notifications(Duration::from_secs(20))
        .unwrap());
    let notifications = engine.take_sql_notifications();
    assert_eq!(notifications.len(), 1);
    let notification = &notifications[0];
    std::fs::write(
        handshake.join(format!("{mode}-result")),
        format!(
            "{}\n{}\n{}\n",
            notification.process_id, notification.channel, notification.payload
        ),
    )
    .unwrap();
}

#[test]
fn separate_processes_deliver_defer_and_reap_notifications() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("cross-process-notifications.db");
    let sender = Engine::open(&database).unwrap();
    let sender_process_id = sender.backend_process_id();
    assert!(notification_registry_path(&database).exists());

    let mut idle = spawn_notification_child(&database, directory.path(), "idle");
    let idle_process_id: i32 = wait_for_notification_value(&directory.path().join("idle-ready"))
        .parse()
        .unwrap();
    assert_ne!(idle_process_id, sender_process_id);
    exec(&sender, "NOTIFY cross_process_events, 'idle delivery'");
    assert_eq!(
        wait_for_notification_result(&directory.path().join("idle-result")),
        (
            sender_process_id,
            "cross_process_events".into(),
            "idle delivery".into()
        )
    );
    assert!(idle.wait().unwrap().success());

    let mut transaction = spawn_notification_child(&database, directory.path(), "transaction");
    let transaction_process_id: i32 =
        wait_for_notification_value(&directory.path().join("transaction-ready"))
            .parse()
            .unwrap();
    assert_ne!(transaction_process_id, sender_process_id);
    exec(
        &sender,
        "NOTIFY cross_process_events, 'transaction delivery'",
    );
    wait_for_notification_file(&directory.path().join("transaction-wait-finished"));
    assert!(!directory.path().join("transaction-result").exists());
    std::fs::write(directory.path().join("release-transaction"), b"1").unwrap();
    assert_eq!(
        wait_for_notification_result(&directory.path().join("transaction-result")),
        (
            sender_process_id,
            "cross_process_events".into(),
            "transaction delivery".into()
        )
    );
    assert!(transaction.wait().unwrap().success());

    let mut crashed = spawn_notification_child(&database, directory.path(), "crash");
    wait_for_notification_file(&directory.path().join("crash-ready"));
    assert!(crashed.wait().unwrap().success());
    exec(&sender, "NOTIFY cross_process_events, 'no recipient'");
    let usage = exec(&sender, "SELECT pg_notification_queue_usage() AS usage");
    assert_eq!(usage.rows[0].get("usage"), Some(&Value::Float(0.0)));
    assert!(!std::fs::read_dir(directory.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".lease")
    }));
}

#[test]
fn idle_poll_reconciles_an_interrupted_notification_commit_marker() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("notification-commit-recovery.db");
    let listener = Engine::open(&database).unwrap();
    let sender = Engine::open(&database).unwrap();
    exec(&listener, "LISTEN recovery_events");

    let registry = notification_registry_path(&database);
    let connection = Connection::open(&registry).unwrap();
    assert_eq!(
        connection
            .execute("UPDATE listeners SET transaction_open = 1", [])
            .unwrap(),
        1
    );
    drop(connection);

    exec(
        &sender,
        "NOTIFY recovery_events, 'after interrupted commit'",
    );
    assert!(listener.take_sql_notifications().is_empty());
    assert_eq!(listener.poll_sql_notifications().unwrap(), 1);
    assert_eq!(
        values(listener.take_sql_notifications()),
        vec![("recovery_events".into(), "after interrupted commit".into())]
    );
    let transaction_open = Connection::open(registry)
        .unwrap()
        .query_row("SELECT transaction_open FROM listeners", [], |row| {
            row.get::<_, bool>(0)
        })
        .unwrap();
    assert!(!transaction_open);
}
