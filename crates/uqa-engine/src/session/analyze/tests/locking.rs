//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analysis locks precede physical writes and follow SQL transaction/savepoint scope.

use super::*;
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn wait_for_relation(writer: &Engine, session: u64, finished: impl Fn() -> bool) -> bool {
    let relation = writer.row_locks.table_key("public.t");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !writer.row_locks.waiting_for_relation(session, relation)
        && !finished()
        && Instant::now() < deadline
    {
        thread::yield_now();
    }
    writer.row_locks.waiting_for_relation(session, relation)
}

#[test]
fn competing_analysis_waits_until_commit_rollback_or_savepoint_undo() {
    for provider in 0..3 {
        for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO before_analysis"] {
            let (_directory, first, second) = sessions(provider);
            first
                .sql("BEGIN READ ONLY; SAVEPOINT before_analysis; ANALYZE t", &[])
                .unwrap();
            let session = second.session_id;
            let cancel = second.runtime.cancellation.clone();
            let (send, done) = mpsc::channel();
            let worker = thread::spawn(move || {
                let result = second.sql("ANALYZE t", &[]);
                let _ = send.send(result);
                second
            });
            let waited = wait_for_relation(&first, session, || worker.is_finished());
            let released = first.sql(finish, &[]);
            let result = done.recv_timeout(Duration::from_secs(30));
            if result.is_err() {
                cancel.cancel();
            }
            let second = worker.join().unwrap();
            released.unwrap();
            assert!(
                waited,
                "provider {provider}, {finish}: analysis bypassed the existing lock"
            );
            result
                .expect("analysis did not finish after lock release")
                .unwrap();
            assert_eq!(persisted_rows(&second), 1);
            if finish.starts_with("ROLLBACK TO") {
                assert_eq!(first.transaction_depth(), 1);
                first.sql("ROLLBACK", &[]).unwrap();
            }
        }
    }
}

#[test]
fn cancelling_a_waiting_analysis_preserves_its_sqlstate_and_releases_its_scope() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        first.sql("BEGIN; ANALYZE t", &[]).unwrap();
        let session = second.session_id;
        let cancel = second.runtime.cancellation.clone();
        let (send, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = second.sql("ANALYZE t", &[]);
            let _ = send.send(result);
            second
        });
        let waited = wait_for_relation(&first, session, || worker.is_finished());
        cancel.cancel();
        let result = done.recv_timeout(Duration::from_secs(30));
        first.sql("COMMIT", &[]).unwrap();
        let second = worker.join().unwrap();
        assert!(waited, "provider {provider}: analysis did not wait");
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(second.transaction_depth(), 0);
        assert!(!first
            .row_locks
            .waiting_for_relation(session, first.row_locks.table_key("public.t")));
        first.sql("ANALYZE t", &[]).unwrap();
    }
}

#[test]
fn named_analysis_rechecks_a_drop_or_replacement_while_acquiring_its_binding_lock() {
    for provider in 0..3 {
        for replace in [false, true] {
            let (_directory, first, second) = sessions(provider);
            first.sql("BEGIN; DROP TABLE t", &[]).unwrap();
            if replace {
                first
                    .sql(
                        "CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (20), (30)",
                        &[],
                    )
                    .unwrap();
            }
            let session = second.session_id;
            let cancel = second.runtime.cancellation.clone();
            let (send, done) = mpsc::channel();
            let worker = thread::spawn(move || {
                let result = second.sql("ANALYZE t", &[]);
                let _ = send.send(result);
                second
            });
            let waited = wait_for_relation(&first, session, || worker.is_finished());
            first.sql("COMMIT", &[]).unwrap();
            let result = done.recv_timeout(Duration::from_secs(30));
            if result.is_err() {
                cancel.cancel();
            }
            let second = worker.join().unwrap();
            assert!(waited, "provider {provider}: name binding bypassed DROP");
            let result = result.unwrap();
            if replace {
                result.unwrap();
                assert_eq!(persisted_rows(&second), 2);
            } else {
                assert_eq!(result.unwrap_err().sqlstate(), Some("42P01"));
            }
        }
    }
}

#[test]
fn automatic_analysis_acquires_its_relation_lock_before_sampling() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        first.sql("BEGIN; ANALYZE t", &[]).unwrap();
        let session = second.session_id;
        let cancel = second.runtime.cancellation.clone();
        let (send, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = second.run_automatic_analyze("public.t");
            let _ = send.send(result);
            second
        });
        let waited = wait_for_relation(&first, session, || worker.is_finished());
        first.sql("COMMIT", &[]).unwrap();
        let result = done.recv_timeout(Duration::from_secs(30));
        if result.is_err() {
            cancel.cancel();
        }
        let second = worker.join().unwrap();
        assert!(
            waited,
            "provider {provider}: automatic sampling bypassed a competing analysis"
        );
        assert!(!result.unwrap().unwrap());
        assert_eq!(second.transaction_depth(), 0);
    }
}

#[test]
fn schema_rename_waits_for_member_statistics_and_refreshes_after_release() {
    for provider in 0..3 {
        for commit in [false, true] {
            let (_directory, first, second) = sessions(provider);
            first
                .sql("BEGIN; INSERT INTO t VALUES (20); ANALYZE t", &[])
                .unwrap();
            let session = second.session_id;
            let cancel = second.runtime.cancellation.clone();
            let (send, done) = mpsc::channel();
            let worker = thread::spawn(move || {
                let result = second.sql("ALTER SCHEMA public RENAME TO renamed", &[]);
                let _ = send.send(result);
                second
            });
            let waited = wait_for_relation(&first, session, || worker.is_finished());
            let released = first.sql(if commit { "COMMIT" } else { "ROLLBACK" }, &[]);
            let result = done.recv_timeout(Duration::from_secs(30));
            if result.is_err() {
                cancel.cancel();
            }
            let second = worker.join().unwrap();
            assert!(
                waited,
                "provider {provider}, commit={commit}: schema rename bypassed member statistics"
            );
            released.unwrap();
            result.unwrap().unwrap();
            let catalog = second.storage.catalog.as_ref().unwrap();
            assert!(catalog.load_column_stats("public.t").unwrap().is_empty());
            let expected = if commit { 2 } else { 1 };
            assert_eq!(
                catalog.load_column_stats("renamed.t").unwrap()[0].row_count,
                expected
            );
            let count = second
                .sql("SELECT count(*) AS n FROM renamed.t", &[])
                .unwrap();
            assert_eq!(count.rows.len(), 1);
            assert_eq!(count.rows[0]["n"], Value::Int(expected));
            second
                .sql("ALTER SCHEMA renamed RENAME TO public", &[])
                .unwrap();
            assert_eq!(persisted_rows(&second), expected);
        }
    }
}
