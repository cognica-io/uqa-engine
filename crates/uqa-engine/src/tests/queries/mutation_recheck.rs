//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, SQLFunctionOptions, SQLFunctionVolatility};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::Duration;
use uqa_core::Value;

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn rejected_join_partner_keeps_later_candidates_available(#[case] provider: Provider) {
    // PostgreSQL 18.4 returns (1, 2, 2) after the other session commits n=2 between qualification and tuple locking. UPDATE evaluates its initial assignments for both candidates and reprojects the surviving recheck, leaving value=3 and the next sequence value=4.
    for delete in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let engine = provider.open(&directory.path().join("join-recheck.db"));
        engine
            .sql(
                "CREATE TABLE target (id integer PRIMARY KEY, n integer, value integer); INSERT INTO target VALUES (1,0,0); CREATE SEQUENCE assignment_ticks",
                &[],
            )
            .unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume_rx = Mutex::new(resume_rx);
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = Arc::clone(&calls);
        engine
            .register_scalar_function_with_options(
                "join_recheck_gate",
                SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                move |args: &[Value]| {
                    if callback_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        entered_tx.send(args[0].clone()).unwrap();
                        resume_rx
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(20))
                            .unwrap();
                    }
                    Ok(Value::Int(1))
                },
            )
            .unwrap();
        let holder = engine.new_session().unwrap();
        let waiter = engine.new_session().unwrap();
        holder.sql("BEGIN; UPDATE target SET n=2", &[]).unwrap();
        let query = if delete {
            "DELETE FROM target t USING (VALUES (1),(2)) s(x) WHERE (t.n=0 OR t.n=s.x) AND join_recheck_gate(s.x)=1 RETURNING t.id,t.n,s.x"
        } else {
            "UPDATE target t SET value=nextval('assignment_ticks') FROM (VALUES (1),(2)) s(x) WHERE (t.n=0 OR t.n=s.x) AND join_recheck_gate(s.x)=1 RETURNING t.id,t.n,s.x"
        };
        let task = std::thread::spawn(move || waiter.sql(query, &[]));
        let first = entered_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        holder.sql("COMMIT", &[]).unwrap();
        resume_tx.send(()).unwrap();
        let result = task.join().unwrap().unwrap();
        assert_eq!(first, Value::Int(1), "{provider:?}, {query}");
        assert_eq!(result.affected_rows, 1, "{provider:?}, {query}");
        for (column, expected) in [1, 2, 2].into_iter().enumerate() {
            assert_eq!(result.value_at(0, column), Some(&Value::Int(expected)));
        }
        let remaining = engine.sql("SELECT id,n,value FROM target", &[]).unwrap();
        if delete {
            assert!(remaining.rows.is_empty());
        } else {
            assert_eq!(remaining.value_at(0, 2), Some(&Value::Int(3)));
            assert_eq!(
                engine
                    .sql("SELECT nextval('assignment_ticks')", &[])
                    .unwrap()
                    .value_at(0, 0),
                Some(&Value::Int(4))
            );
        }
    }
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn plain_update_assignments_precede_locking_and_repeat_only_for_qualifying_rechecks(
    #[case] provider: Provider,
) {
    // PostgreSQL 18.4 consumes one sequence value before locking and a second only for a successful recheck: stored value=2/next=3 on success, unchanged value=0/next=2 on rejection.
    for assignment_gate in [false, true] {
        for reject in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let engine = provider.open(&directory.path().join("plain-update-recheck.db"));
            engine.sql("CREATE TABLE target(id integer PRIMARY KEY,n integer,value integer); INSERT INTO target VALUES(1,0,0); CREATE SEQUENCE assignment_ticks", &[]).unwrap();
            let (entered_tx, entered_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let resume_rx = Mutex::new(resume_rx);
            let calls = AtomicUsize::new(0);
            engine
                .register_scalar_function_with_options(
                    "plain_recheck_gate",
                    SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                    move |args: &[Value]| {
                        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                            entered_tx.send(args[0].clone()).unwrap();
                            resume_rx
                                .lock()
                                .unwrap()
                                .recv_timeout(Duration::from_secs(20))
                                .unwrap();
                        }
                        Ok(args[0].clone())
                    },
                )
                .unwrap();
            let holder = engine.new_session().unwrap();
            let waiter = engine.new_session().unwrap();
            holder.sql("BEGIN; UPDATE target SET n=2", &[]).unwrap();
            let value = if assignment_gate {
                "plain_recheck_gate(nextval('assignment_ticks'))"
            } else {
                "nextval('assignment_ticks')"
            };
            let predicate = if assignment_gate {
                "n>=0"
            } else {
                "plain_recheck_gate(n)>=0"
            };
            let query = format!(
                "UPDATE target SET value={value} WHERE {predicate}{} RETURNING id,n,value",
                if reject { " AND n=0" } else { "" }
            );
            let task = std::thread::spawn(move || waiter.sql(&query, &[]));
            let first = entered_rx.recv_timeout(Duration::from_secs(20));
            // Always release the writer and worker before asserting the expected execution order.
            holder.sql("COMMIT", &[]).unwrap();
            let _ = resume_tx.send(());
            let result = task.join().unwrap().unwrap();
            assert_eq!(
                first.unwrap(),
                Value::Int(i64::from(assignment_gate)),
                "{provider:?}, assignment_gate={assignment_gate}, reject={reject}"
            );
            assert_eq!(result.affected_rows, u64::from(!reject));
            let stored = engine
                .sql(
                    "SELECT n,value,nextval('assignment_ticks') FROM target",
                    &[],
                )
                .unwrap();
            for (column, expected) in [2, if reject { 0 } else { 2 }, if reject { 2 } else { 3 }]
                .into_iter()
                .enumerate()
            {
                assert_eq!(
                    stored.value_at(0, column),
                    Some(&Value::Int(expected)),
                    "{provider:?}, assignment_gate={assignment_gate}, reject={reject}"
                );
            }
        }
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn point_update_selection_does_not_evaluate_assignments_speculatively(#[case] provider: Provider) {
    // PostgreSQL 18.4 leaves nextval=1 for an absent target and nextval=2 for one updated target, including CHECK and row-dependent fallback paths.
    for (suffix, predicate, assignments, expected) in [
        ("", "id=999", "value=7", 1),
        ("", "id=1", "value=7", 2),
        (", CHECK(value>=0)", "id=1", "value=7", 2),
        ("", "id=1", "value=7,id=id", 2),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let engine = provider.open(&directory.path().join("point-update-assignment.db"));
        engine.sql(&format!("CREATE SEQUENCE assignment_ticks; CREATE DOMAIN traced_integer AS bigint CHECK(nextval('assignment_ticks')>0); CREATE TABLE target(id integer PRIMARY KEY,value traced_integer{suffix}); INSERT INTO target VALUES(1,0); ALTER SEQUENCE assignment_ticks RESTART WITH 1"), &[]).unwrap();
        engine
            .sql(
                &format!("UPDATE target SET {assignments} WHERE {predicate}"),
                &[],
            )
            .unwrap();
        assert_eq!(
            engine
                .sql("SELECT nextval('assignment_ticks')", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Int(expected)),
            "{provider:?}, {suffix}, {assignments}, {predicate}"
        );
    }
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn point_update_rechecks_repeat_only_the_selected_target_assignment(#[case] provider: Provider) {
    // PostgreSQL 18.4 yields (id,n,value,nextval)=(1,2,7,3) after an accepted recheck and (2,2,0,2) after the key changes while the first domain check waits.
    for reject in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let engine = provider.open(&directory.path().join("point-update-recheck.db"));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume_rx = Mutex::new(resume_rx);
        let armed = Arc::new(AtomicBool::new(false));
        let callback_armed = Arc::clone(&armed);
        engine
            .register_scalar_function_with_options(
                "point_recheck_gate",
                SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                move |args: &[Value]| {
                    if callback_armed.swap(false, Ordering::SeqCst) {
                        entered_tx.send(args[0].clone()).unwrap();
                        resume_rx
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(20))
                            .unwrap();
                    }
                    Ok(args[0].clone())
                },
            )
            .unwrap();
        engine.sql("CREATE SEQUENCE assignment_ticks; CREATE FUNCTION point_checked(n bigint) RETURNS bigint LANGUAGE plpgsql AS $$ BEGIN RETURN point_recheck_gate(n); END $$; CREATE DOMAIN traced_integer AS bigint CHECK(point_checked(nextval('assignment_ticks'))>0); CREATE TABLE target(id integer PRIMARY KEY,n integer,value traced_integer); INSERT INTO target VALUES(1,0,0); ALTER SEQUENCE assignment_ticks RESTART WITH 1", &[]).unwrap();
        let holder = engine.new_session().unwrap();
        let waiter = engine.new_session().unwrap();
        holder
            .sql(
                if reject {
                    "BEGIN; UPDATE target SET id=2,n=2"
                } else {
                    "BEGIN; UPDATE target SET n=2"
                },
                &[],
            )
            .unwrap();
        armed.store(true, Ordering::SeqCst);
        let task =
            std::thread::spawn(move || waiter.sql("UPDATE target SET value=7 WHERE id=1", &[]));
        let first = entered_rx.recv_timeout(Duration::from_secs(20));
        holder.sql("COMMIT", &[]).unwrap();
        let _ = resume_tx.send(());
        let result = task.join().unwrap().unwrap();
        assert_eq!(first.unwrap(), Value::Int(1));
        assert_eq!(result.affected_rows, u64::from(!reject));
        let stored = engine
            .sql(
                "SELECT n,value,nextval('assignment_ticks') FROM target",
                &[],
            )
            .unwrap();
        for (column, expected) in [2, if reject { 0 } else { 7 }, if reject { 2 } else { 3 }]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                stored.value_at(0, column),
                Some(&Value::Int(expected)),
                "{provider:?}, reject={reject}"
            );
        }
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn point_update_not_null_failure_keeps_single_assignment_and_complete_row_diagnostics(
    #[case] provider: Provider,
) {
    // Independently captured PostgreSQL 18.4: 23502, "Failing row contains (1, null).", retained value=0 and nextval=2 after ROLLBACK TO.
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("point-update-null.db"));
    engine.sql("CREATE SEQUENCE assignment_ticks; CREATE DOMAIN traced_integer AS bigint CHECK(nextval('assignment_ticks')>0); CREATE TABLE target(id integer PRIMARY KEY,value traced_integer NOT NULL); INSERT INTO target VALUES(1,0); ALTER SEQUENCE assignment_ticks RESTART WITH 1; BEGIN; SAVEPOINT before_update", &[]).unwrap();
    let error = engine
        .sql("UPDATE target SET value=NULL WHERE id=1", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23502"));
    assert_eq!(error.detail(), Some("Failing row contains (1, null)."));
    engine.sql("ROLLBACK TO before_update", &[]).unwrap();
    let stored = engine
        .sql("SELECT value,nextval('assignment_ticks') FROM target", &[])
        .unwrap();
    assert_eq!(stored.value_at(0, 0), Some(&Value::Int(0)));
    assert_eq!(stored.value_at(0, 1), Some(&Value::Int(2)));
    engine.sql("COMMIT", &[]).unwrap();
}
