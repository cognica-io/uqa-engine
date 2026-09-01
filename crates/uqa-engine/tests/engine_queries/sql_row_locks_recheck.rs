//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 tuple-recheck, candidate-preservation, and cross-process row-lock coverage.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc, Barrier,
};
use std::time::{Duration, Instant};

use uqa_core::Value;
use uqa_engine::{Engine, SQLFunctionOptions, SQLFunctionVolatility};

use super::sql_row_locks::{seed_accounts, sqlstate};

#[path = "sql_row_locks_recheck/basic.rs"]
mod basic;
#[path = "sql_row_locks_recheck/update_chains.rs"]
mod update_chains;
#[path = "sql_row_locks_recheck/writer_order.rs"]
mod writer_order;

/// Child half of the cross-process coordination test. Runs only when the parent test re-executes this binary with the handshake environment set; otherwise it passes vacuously.
#[test]
fn cross_process_lock_holder_child() {
    let Ok(database) = std::env::var("UQA_CROSS_LOCK_CHILD_DB") else {
        return;
    };
    let handshake = std::path::PathBuf::from(std::env::var("UQA_CROSS_LOCK_CHILD_DIR").unwrap());
    let engine = Engine::open(std::path::Path::new(&database)).unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    std::fs::write(handshake.join("child-holds-lock"), b"1").unwrap();
    let release = handshake.join("release-child");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !release.exists() {
        assert!(Instant::now() < deadline, "parent never released the child");
        std::thread::sleep(Duration::from_millis(20));
    }
    engine
        .sql("UPDATE accounts SET balance = 901 WHERE id = 1", &[])
        .unwrap();
    engine.sql("COMMIT", &[]).unwrap();
}

fn spawn_cross_process_child(
    test_name: &str,
    database: &std::path::Path,
    handshake: &std::path::Path,
) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env("UQA_CROSS_LOCK_CHILD_DB", database)
        .env("UQA_CROSS_LOCK_CHILD_DIR", handshake)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap()
}

fn wait_for_file(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "handshake file {} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_file_contents(path: &std::path::Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match std::fs::read_to_string(path) {
            Ok(contents) if !contents.is_empty() => return contents,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("failed to read handshake file {}: {error}", path.display()),
        }
        assert!(
            Instant::now() < deadline,
            "handshake file {} never received contents",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn publish_file_contents(path: &std::path::Path, contents: impl AsRef<[u8]>) {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, contents).unwrap();
    std::fs::rename(temporary, path).unwrap();
}

#[test]
fn separate_processes_coordinate_row_locks() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("cross-process.db");
    {
        let engine = Engine::open(&database).unwrap();
        seed_accounts(&engine);
    }
    let mut child = spawn_cross_process_child(
        "engine_queries::sql_row_locks_recheck::cross_process_lock_holder_child",
        &database,
        directory.path(),
    );
    wait_for_file(&directory.path().join("child-holds-lock"));

    let engine = Engine::open(&database).unwrap();
    let error = engine
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    let skipped = engine
        .sql(
            "SELECT id FROM accounts WHERE id <= 2 ORDER BY id FOR UPDATE SKIP LOCKED",
            &[],
        )
        .unwrap();
    assert_eq!(
        skipped
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(2)]
    );

    let (done_tx, done_rx) = mpsc::channel();
    let waiter_database = database.clone();
    let waiting_thread = std::thread::spawn(move || {
        let waiter = Engine::open(&waiter_database).unwrap();
        done_tx
            .send(waiter.sql(
                "SELECT id, balance FROM accounts WHERE id = 1 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(300)).is_err());
    std::fs::write(directory.path().join("release-child"), b"1").unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("balance"), Some(&Value::Int(901)));
    assert!(child.wait().unwrap().success());
    assert!(database
        .with_file_name("cross-process.db.uqa-locks")
        .exists());
}

#[test]
fn cross_process_idle_attachment_child() {
    let Ok(database) = std::env::var("UQA_CROSS_IDLE_CHILD_DB") else {
        return;
    };
    let handshake = std::path::PathBuf::from(std::env::var("UQA_CROSS_IDLE_CHILD_DIR").unwrap());
    let _engine = Engine::open(std::path::Path::new(&database)).unwrap();
    std::fs::write(handshake.join("idle-child-ready"), b"1").unwrap();
    wait_for_file(&handshake.join("release-idle-child"));
}

#[test]
fn stale_cross_process_rewrite_does_not_capture_a_reused_primary_key() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("cross-stale-rewrite.db");
    let engine = Engine::open(&database).unwrap();
    engine
        .sql(
            "CREATE TABLE recycled (id INTEGER PRIMARY KEY, value TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO recycled VALUES (1, 'old')", &[])
        .unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("engine_queries::sql_row_locks_recheck::cross_process_idle_attachment_child")
        .arg("--nocapture")
        .env("UQA_CROSS_IDLE_CHILD_DB", &database)
        .env("UQA_CROSS_IDLE_CHILD_DIR", directory.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    wait_for_file(&directory.path().join("idle-child-ready"));

    engine
        .sql("UPDATE recycled SET id = 2 WHERE id = 1", &[])
        .unwrap();
    engine
        .sql("DELETE FROM recycled WHERE id = 2", &[])
        .unwrap();
    engine
        .sql("INSERT INTO recycled VALUES (1, 'fresh')", &[])
        .unwrap();
    let result = engine
        .sql("SELECT id, value FROM recycled FOR UPDATE", &[])
        .unwrap();

    std::fs::write(directory.path().join("release-idle-child"), b"1").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(1)));
    assert_eq!(
        result.rows[0].get("value"),
        Some(&Value::Str("fresh".into()))
    );
}

#[test]
fn cross_process_exit_writer_child() {
    let Ok(database) = std::env::var("UQA_CROSS_EXIT_WRITER_DB") else {
        return;
    };
    let handshake = std::path::PathBuf::from(std::env::var("UQA_CROSS_EXIT_WRITER_DIR").unwrap());
    wait_for_file(&handshake.join("reader-at-gate"));
    let engine = Engine::open(std::path::Path::new(&database)).unwrap();
    engine
        .sql("UPDATE exit_writer SET value = 'new' WHERE id = 1", &[])
        .unwrap();
    std::fs::write(handshake.join("exit-writer-done"), b"1").unwrap();
}

#[test]
fn row_lock_rechecks_a_commit_after_the_external_writer_exits() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("cross-exited-writer.db");
    let engine = Engine::open(&database).unwrap();
    engine
        .sql(
            "CREATE TABLE exit_writer (id INTEGER PRIMARY KEY, value TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO exit_writer VALUES (1, 'old')", &[])
        .unwrap();
    let handshake = directory.path().to_path_buf();
    engine
        .register_scalar_function_with_options(
            "cross_process_exit_gate",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |_args: &[Value]| {
                std::fs::write(handshake.join("reader-at-gate"), b"1").unwrap();
                wait_for_file(&handshake.join("exit-writer-done"));
                Ok(Value::Int(1))
            },
        )
        .unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("engine_queries::sql_row_locks_recheck::cross_process_exit_writer_child")
        .arg("--nocapture")
        .env("UQA_CROSS_EXIT_WRITER_DB", &database)
        .env("UQA_CROSS_EXIT_WRITER_DIR", directory.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();

    let result = engine
        .sql(
            "SELECT id, value, cross_process_exit_gate() AS gate FROM exit_writer ORDER BY gate FOR UPDATE",
            &[],
        )
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("value"), Some(&Value::Str("new".into())));
}

/// Child half of the cross-process deadlock test: locks row 2, then blocks requesting row 1 while the parent holds row 1 and requests row 2.
#[test]
fn cross_process_deadlock_child() {
    let Ok(database) = std::env::var("UQA_CROSS_DEADLOCK_CHILD_DB") else {
        return;
    };
    let handshake =
        std::path::PathBuf::from(std::env::var("UQA_CROSS_DEADLOCK_CHILD_DIR").unwrap());
    let engine = Engine::open(std::path::Path::new(&database)).unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[])
        .unwrap();
    std::fs::write(handshake.join("child-holds-row2"), b"1").unwrap();
    wait_for_file(&handshake.join("parent-holds-row1"));
    let outcome = engine.sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[]);
    let outcome_tag = match &outcome {
        Ok(_) => "granted".to_string(),
        Err(error) => sqlstate(error).to_string(),
    };
    publish_file_contents(&handshake.join("child-outcome"), outcome_tag);
    engine.sql("ROLLBACK", &[]).ok();
}

#[test]
fn separate_processes_detect_lock_cycles() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("cross-deadlock.db");
    {
        let engine = Engine::open(&database).unwrap();
        seed_accounts(&engine);
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("engine_queries::sql_row_locks_recheck::cross_process_deadlock_child")
        .arg("--nocapture")
        .env("UQA_CROSS_DEADLOCK_CHILD_DB", &database)
        .env("UQA_CROSS_DEADLOCK_CHILD_DIR", directory.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    wait_for_file(&directory.path().join("child-holds-row2"));

    let engine = Engine::open(&database).unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    std::fs::write(directory.path().join("parent-holds-row1"), b"1").unwrap();
    let parent_outcome = engine.sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[]);
    let parent_tag = match &parent_outcome {
        Ok(_) => "granted".to_string(),
        Err(error) => sqlstate(error).to_string(),
    };
    engine.sql("ROLLBACK", &[]).ok();
    let child_tag = wait_for_file_contents(&directory.path().join("child-outcome"));
    assert!(child.wait().unwrap().success());
    assert!(
        parent_tag == "40P01" || child_tag == "40P01",
        "one side must detect the cross-process deadlock; parent: {parent_tag}, child: {child_tag}"
    );
}

#[test]
fn mixed_process_deadlock_child() {
    let Ok(database) = std::env::var("UQA_MIXED_DEADLOCK_CHILD_DB") else {
        return;
    };
    let handshake =
        std::path::PathBuf::from(std::env::var("UQA_MIXED_DEADLOCK_CHILD_DIR").unwrap());
    let engine = Engine::open(std::path::Path::new(&database)).unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("SELECT id FROM accounts WHERE id = 3 FOR UPDATE", &[])
        .unwrap();
    std::fs::write(handshake.join("mixed-child-holds-row3"), b"1").unwrap();
    wait_for_file(&handshake.join("mixed-child-request-row1"));
    let outcome = engine.sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[]);
    let outcome_tag = match &outcome {
        Ok(_) => "granted".to_string(),
        Err(error) => sqlstate(error).to_string(),
    };
    publish_file_contents(&handshake.join("mixed-child-outcome"), outcome_tag);
    engine.sql("ROLLBACK", &[]).ok();
}

/// Observe the durable wait-slot file from a third process. Opening the sidecar in either lock-owning process would release that process's POSIX record locks when the descriptor closes, so the parent delegates this handshake to a process that owns no database locks.
#[test]
fn cross_process_wait_slot_observer_child() {
    let Ok(database) = std::env::var("UQA_WAIT_SLOT_OBSERVER_DB") else {
        return;
    };
    let waiting_pid = std::env::var("UQA_WAIT_SLOT_OBSERVER_PID")
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let ready = std::path::PathBuf::from(std::env::var("UQA_WAIT_SLOT_OBSERVER_READY").unwrap());
    let mut sidecar = std::ffi::OsString::from(database);
    sidecar.push(".uqa-locks");
    let mut file = std::fs::File::open(std::path::PathBuf::from(sidecar)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        use std::io::{Read as _, Seek as _, SeekFrom};

        file.seek(SeekFrom::Start(64)).unwrap();
        for _ in 0..256 {
            let mut slot = [0_u8; 32];
            match file.read_exact(&mut slot) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(error) => panic!("failed to read the lock sidecar: {error}"),
            }
            if u32::from_be_bytes(slot[0..4].try_into().unwrap()) == 0x5551_4c4b
                && u32::from_be_bytes(slot[4..8].try_into().unwrap()) == waiting_pid
            {
                std::fs::write(ready, b"1").unwrap();
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "cross-process row-lock waiter was never registered"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn separate_processes_detect_cycles_that_include_a_local_wait_edge() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("mixed-cross-deadlock.db");
    {
        let engine = Engine::open(&database).unwrap();
        seed_accounts(&engine);
    }
    let session_a = Engine::open(&database).unwrap();
    let session_b = session_a.new_session().unwrap();
    session_a.sql("BEGIN", &[]).unwrap();
    session_b.sql("BEGIN", &[]).unwrap();
    session_a
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    session_b
        .sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[])
        .unwrap();

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("engine_queries::sql_row_locks_recheck::mixed_process_deadlock_child")
        .arg("--nocapture")
        .env("UQA_MIXED_DEADLOCK_CHILD_DB", &database)
        .env("UQA_MIXED_DEADLOCK_CHILD_DIR", directory.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let child_pid = child.id();
    wait_for_file(&directory.path().join("mixed-child-holds-row3"));

    let (done_tx, done_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let outcome = session_b.sql("SELECT id FROM accounts WHERE id = 3 FOR UPDATE", &[]);
        let outcome_tag = match &outcome {
            Ok(_) => "granted".to_string(),
            Err(error) => sqlstate(error),
        };
        session_b.sql("ROLLBACK", &[]).ok();
        done_tx.send(outcome_tag).unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    std::fs::write(directory.path().join("mixed-child-request-row1"), b"1").unwrap();
    let observer_ready = directory.path().join("mixed-child-waits-row1");
    let mut observer = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("engine_queries::sql_row_locks_recheck::cross_process_wait_slot_observer_child")
        .arg("--nocapture")
        .env("UQA_WAIT_SLOT_OBSERVER_DB", &database)
        .env("UQA_WAIT_SLOT_OBSERVER_PID", child_pid.to_string())
        .env("UQA_WAIT_SLOT_OBSERVER_READY", &observer_ready)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    wait_for_file(&observer_ready);
    assert!(observer.wait().unwrap().success());

    let (session_a_tx, session_a_rx) = mpsc::channel();
    let session_a_waiter = std::thread::spawn(move || {
        let outcome = session_a.sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[]);
        let outcome_tag = match &outcome {
            Ok(_) => "granted".to_string(),
            Err(error) => sqlstate(error),
        };
        session_a.sql("ROLLBACK", &[]).ok();
        session_a_tx.send(outcome_tag).unwrap();
    });
    let parent_outcome = session_a_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let waiter_outcome = done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let child_outcome = wait_for_file_contents(&directory.path().join("mixed-child-outcome"));
    assert!(child.wait().unwrap().success());
    session_a_waiter.join().unwrap();
    waiter.join().unwrap();
    let outcomes = [parent_outcome, waiter_outcome, child_outcome];
    assert!(
        outcomes.iter().any(|outcome| outcome == "40P01"),
        "one side must detect the mixed-process deadlock; outcomes: {outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome == "granted" || outcome == "40P01"),
        "every mixed-process deadlock participant must either be granted or chosen as a victim; outcomes: {outcomes:?}"
    );
}
