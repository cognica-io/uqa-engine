//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit SQL table locks across every persistent provider and session boundary.

use super::relation_lock_support::{error, sessions, sql, wait_for_relation};
use super::*;
use std::{sync::mpsc, thread, time::Duration};

const MODES: [&str; 8] = [
    "ACCESS SHARE",
    "ROW SHARE",
    "ROW EXCLUSIVE",
    "SHARE UPDATE EXCLUSIVE",
    "SHARE",
    "SHARE ROW EXCLUSIVE",
    "EXCLUSIVE",
    "ACCESS EXCLUSIVE",
];

#[test]
fn all_table_lock_modes_match_conflicts_and_read_only_transactions() {
    let conflicts = [
        ".......X", "......XX", "....XXXX", "...XXXXX", "..XX.XXX", "..XXXXXX", ".XXXXXXX",
        "XXXXXXXX",
    ];
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        for (held, expected) in MODES.into_iter().zip(conflicts) {
            sql(&first, &format!("BEGIN READ ONLY; LOCK t IN {held} MODE"));
            for (wanted, conflict) in MODES.into_iter().zip(expected.bytes()) {
                sql(&second, "BEGIN READ ONLY");
                let statement = format!("LOCK t IN {wanted} MODE NOWAIT");
                if conflict == b'X' {
                    error(&second, &statement, "55P03");
                    error(&second, "SELECT 1", "25P02");
                } else {
                    assert_eq!(
                        sql(&second, &statement).command_tag.as_deref(),
                        Some("LOCK TABLE")
                    );
                }
                sql(&second, "ROLLBACK");
            }
            sql(&first, "ROLLBACK");
        }
    }
}

#[test]
fn table_lock_transaction_context_and_wrong_target_errors_match_postgresql() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE t(v integer); CREATE VIEW v AS SELECT * FROM t; CREATE MATERIALIZED VIEW m AS SELECT * FROM t; CREATE SEQUENCE s; CREATE INDEX t_idx ON t(v)");
    error(&engine, "LOCK t", "25P01");
    error(&engine, "LOCK absent", "25P01");
    sql(&engine, "LOCK t; SELECT 1");
    sql(&engine, "SELECT 1; LOCK t");
    sql(&engine, "DO $$BEGIN LOCK t; END$$");
    for (target, state) in [
        ("m", "42809"),
        ("s", "42809"),
        ("t_idx", "42809"),
        ("absent", "42P01"),
        ("absent_schema.t", "3F000"),
    ] {
        sql(&engine, "BEGIN");
        error(&engine, &format!("LOCK {target}"), state);
        sql(&engine, "ROLLBACK");
    }
    sql(&engine, "BEGIN; LOCK v; COMMIT");
}

#[test]
fn table_lock_acl_checks_each_mode_and_ignores_column_only_grants() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE t(v integer); CREATE ROLE lock_user; GRANT USAGE ON SCHEMA public TO lock_user");
    for (privilege, allowed) in [
        ("SELECT", 1),
        ("INSERT", 3),
        ("UPDATE", 8),
        ("DELETE", 8),
        ("TRUNCATE", 8),
        ("MAINTAIN", 8),
        ("REFERENCES", 0),
        ("TRIGGER", 0),
        ("SELECT(v)", 0),
    ] {
        sql(&engine, &format!("GRANT {privilege} ON t TO lock_user"));
        sql(&engine, "SET ROLE lock_user");
        for (index, mode) in MODES.into_iter().enumerate() {
            sql(&engine, "BEGIN");
            let statement = format!("LOCK t IN {mode} MODE");
            if index < allowed {
                sql(&engine, &statement);
            } else {
                error(&engine, &statement, "42501");
            }
            sql(&engine, "ROLLBACK");
        }
        sql(&engine, "RESET ROLE");
        sql(&engine, &format!("REVOKE {privilege} ON t FROM lock_user"));
    }
}

#[test]
fn table_lock_view_security_and_inheritance_preserve_only_and_owner_scope() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS(t); CREATE VIEW v AS SELECT * FROM t; CREATE VIEW only_v AS SELECT * FROM ONLY t; CREATE VIEW invoker_v WITH (security_invoker=true) AS SELECT * FROM t; CREATE ROLE reader; GRANT SELECT ON v, only_v, invoker_v TO reader");
        for target in ["ONLY t", "only_v"] {
            sql(&first, &format!("BEGIN; LOCK {target} IN SHARE MODE"));
            sql(
                &second,
                "BEGIN; LOCK child IN ROW EXCLUSIVE MODE NOWAIT; ROLLBACK",
            );
            sql(&second, "BEGIN");
            error(&second, "LOCK ONLY t IN ROW EXCLUSIVE MODE NOWAIT", "55P03");
            sql(&second, "ROLLBACK");
            sql(&first, "ROLLBACK");
        }
        for target in ["t", "v"] {
            sql(&first, &format!("BEGIN; LOCK {target} IN SHARE MODE"));
            sql(&second, "BEGIN");
            error(&second, "LOCK child IN ROW EXCLUSIVE MODE NOWAIT", "55P03");
            sql(&second, "ROLLBACK");
            sql(&first, "ROLLBACK");
        }
        sql(
            &first,
            "SET ROLE reader; BEGIN; LOCK v IN ACCESS SHARE MODE; COMMIT",
        );
        sql(&first, "BEGIN");
        error(&first, "LOCK invoker_v IN ACCESS SHARE MODE", "42501");
        sql(&first, "ROLLBACK; RESET ROLE");
    }
}

#[test]
fn table_locks_wait_until_commit_rollback_and_savepoint_undo() {
    for provider in 0..3 {
        for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO before_lock"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "BEGIN; SAVEPOINT before_lock; LOCK t IN SHARE MODE");
            let session = second.session_id;
            let cancel = second.runtime.cancellation.clone();
            let (send, done) = mpsc::channel();
            let worker = thread::spawn(move || {
                let result = second.sql("BEGIN; LOCK t IN ROW EXCLUSIVE MODE; COMMIT", &[]);
                let _ = send.send(result);
                second
            });
            let waited = wait_for_relation(&first, session, "public.t", || worker.is_finished());
            sql(&first, finish);
            let result = done.recv_timeout(Duration::from_secs(30));
            if result.is_err() {
                cancel.cancel();
            }
            worker.join().unwrap();
            assert!(waited, "provider {provider}, {finish}");
            result.unwrap().unwrap();
            if finish.starts_with("ROLLBACK TO") {
                sql(&first, "ROLLBACK");
            }
        }
    }
}

#[test]
fn table_lock_rebinds_after_concurrent_drop_and_replacement() {
    for provider in 0..3 {
        for replace in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "BEGIN; DROP TABLE t");
            if replace {
                sql(&first, "CREATE TABLE t(v integer)");
            }
            let session = second.session_id;
            let cancel = second.runtime.cancellation.clone();
            let (send, done) = mpsc::channel();
            let worker = thread::spawn(move || {
                let result = second.sql("BEGIN; LOCK t IN SHARE MODE", &[]);
                let _ = send.send(result);
                second
            });
            let waited = wait_for_relation(&first, session, "public.t", || worker.is_finished());
            sql(&first, "COMMIT");
            let result = done.recv_timeout(Duration::from_secs(30));
            if result.is_err() {
                cancel.cancel();
            }
            let second = worker.join().unwrap();
            assert!(waited, "provider {provider}, replacement {replace}");
            let result = result.unwrap();
            if replace {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().sqlstate(), Some("42P01"));
            }
            sql(&second, "ROLLBACK");
        }
    }
}

#[test]
fn inherited_table_lock_follows_a_child_renamed_while_waiting() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE child() INHERITS (t)");
        sql(&first, "BEGIN; ALTER TABLE child RENAME TO renamed_child");
        let session = second.session_id;
        let cancel = second.runtime.cancellation.clone();
        let (send, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = second.sql("BEGIN; LOCK t IN SHARE MODE", &[]);
            let _ = send.send(result);
            second
        });
        let waited = wait_for_relation(&first, session, "public.child", || worker.is_finished());
        sql(&first, "COMMIT");
        let result = done.recv_timeout(Duration::from_secs(30));
        if result.is_err() {
            cancel.cancel();
        }
        let second = worker.join().unwrap();
        assert!(waited, "provider {provider}");
        result.unwrap().unwrap();
        sql(&first, "BEGIN");
        error(
            &first,
            "LOCK renamed_child IN ROW EXCLUSIVE MODE NOWAIT",
            "55P03",
        );
        sql(&first, "ROLLBACK");
        sql(&second, "COMMIT");
        sql(
            &first,
            "BEGIN; LOCK renamed_child IN ROW EXCLUSIVE MODE NOWAIT; COMMIT",
        );
    }
}

#[test]
fn table_lock_does_not_freeze_the_first_repeatable_read_data_snapshot() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "BEGIN; LOCK t IN ACCESS SHARE MODE; SET TRANSACTION ISOLATION LEVEL REPEATABLE READ",
        );
        sql(&second, "INSERT INTO t VALUES (2)");
        assert_eq!(
            sql(&first, "SELECT count(*) AS n FROM t").rows[0]["n"],
            Value::Int(2)
        );
        sql(&second, "INSERT INTO t VALUES (3)");
        assert_eq!(
            sql(&first, "SELECT count(*) AS n FROM t").rows[0]["n"],
            Value::Int(2)
        );
        sql(&first, "COMMIT");
    }
}

#[test]
fn cancelling_a_table_lock_wait_aborts_the_transaction_and_cleans_its_wait_state() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "BEGIN; LOCK t IN SHARE MODE");
        let session = second.session_id;
        let cancel = second.runtime.cancellation.clone();
        let (send, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = second.sql("BEGIN; LOCK t IN ROW EXCLUSIVE MODE", &[]);
            let _ = send.send(result);
            second
        });
        let waited = wait_for_relation(&first, session, "public.t", || worker.is_finished());
        cancel.cancel();
        let result = done.recv_timeout(Duration::from_secs(30));
        sql(&first, "ROLLBACK");
        let second = worker.join().unwrap();
        assert!(waited, "provider {provider}");
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014"));
        assert!(second.transaction_failed());
        assert!(!first
            .row_locks
            .waiting_for_relation(session, first.row_locks.table_key("public.t")));
    }
}
