//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence value calls retain the original relation and top-level transaction lock.

use crate::{
    tests::relation_lock_support::{after_wait, sessions, sql},
    Engine,
};
use uqa_core::Value;
use uqa_execution::row_locks::{binding::RelationLockSession, RelationLockMode};

fn peer_lock(engine: &Engine, mode: RelationLockMode, allowed: bool) {
    sql(engine, "BEGIN");
    let acquired = {
        let guard = RelationLockSession::acquire(engine, "public.ids", mode, true).unwrap();
        guard.is_some()
    };
    sql(engine, "ROLLBACK");
    assert_eq!(acquired, allowed, "sequence peer lock: {mode:?}");
}

#[test]
fn value_functions_hold_row_exclusive_locks_across_savepoint_rollback() {
    for provider in 0..3 {
        for expression in [
            "nextval('ids')",
            "currval('ids')",
            "lastval()",
            "setval('ids',42,false)",
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE SEQUENCE ids CACHE 5; SELECT nextval('ids')");
            sql(
                &first,
                &format!("BEGIN; SAVEPOINT before_value; SELECT {expression}"),
            );
            peer_lock(&second, RelationLockMode::AccessShare, true);
            peer_lock(&second, RelationLockMode::Share, false);
            sql(&first, "ROLLBACK TO before_value");
            peer_lock(&second, RelationLockMode::Share, false);
            sql(&first, "ROLLBACK");
            peer_lock(&second, RelationLockMode::AccessExclusive, true);
        }
    }
}

#[test]
fn cached_nextval_waits_for_restart_and_uses_the_new_definition() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE SEQUENCE ids CACHE 5");
        sql(&second, "SELECT nextval('ids')");
        sql(&first, "BEGIN; ALTER SEQUENCE ids RESTART 100");
        let (_, result) = after_wait(
            &first,
            second,
            "SELECT nextval('ids') AS v",
            "public.ids",
            "COMMIT",
        );
        assert_eq!(result.unwrap().rows[0]["v"], Value::Int(100));
    }
}

#[test]
fn value_waits_follow_original_sequence_identity_through_name_reuse() {
    for provider in 0..3 {
        for (expression, expected) in [
            ("nextval('ids')", 2),
            ("currval('ids')", 1),
            ("lastval()", 1),
            ("setval('ids',42,false)", 42),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE SEQUENCE ids");
            sql(&second, "SELECT nextval('ids')");
            sql(
                &first,
                "BEGIN; ALTER SEQUENCE ids RENAME TO original; CREATE SEQUENCE ids START 1000",
            );
            let (second, result) = after_wait(
                &first,
                second,
                &format!("SELECT {expression} AS v"),
                "public.ids",
                "COMMIT",
            );
            assert_eq!(result.unwrap().rows[0]["v"], Value::Int(expected));
            if expression.starts_with("setval") {
                assert_eq!(
                    sql(&second, "SELECT nextval('original') AS v").rows[0]["v"],
                    Value::Int(42)
                );
            }
            assert_eq!(
                sql(&second, "SELECT nextval('ids') AS v").rows[0]["v"],
                Value::Int(1000)
            );
        }
    }
}

#[test]
fn a_sequence_removed_during_a_value_wait_is_not_replaced_by_its_old_name() {
    for provider in 0..3 {
        for expression in ["nextval('ids')", "lastval()"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE SEQUENCE ids");
            sql(&second, "SELECT nextval('ids')");
            sql(
                &first,
                "BEGIN; DROP SEQUENCE ids; CREATE SEQUENCE ids START 1000",
            );
            let (second, result) = after_wait(
                &first,
                second,
                &format!("SELECT {expression}"),
                "public.ids",
                "COMMIT",
            );
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some("XX000"),
                "{provider}/{expression}: {error}"
            );
            assert_eq!(
                sql(&second, "SELECT nextval('ids') AS v").rows[0]["v"],
                Value::Int(1000)
            );
        }
    }
}

#[test]
fn sequence_value_authority_is_checked_after_the_definition_wait() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE SEQUENCE ids; CREATE ROLE reader; GRANT USAGE ON SEQUENCE ids TO reader",
        );
        sql(&second, "SET ROLE reader");
        sql(
            &first,
            "BEGIN; ALTER SEQUENCE ids INCREMENT 2; REVOKE USAGE ON SEQUENCE ids FROM reader",
        );
        let (_, result) = after_wait(
            &first,
            second,
            "SELECT nextval('ids')",
            "public.ids",
            "COMMIT",
        );
        let error = result.unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"), "{provider}: {error}");
    }
}

#[test]
fn retaining_a_sequence_value_lock_does_not_extend_an_earlier_definition_lock() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE SEQUENCE ids");
        sql(&first, "BEGIN; SAVEPOINT before_definition; ALTER SEQUENCE ids CACHE 5; SELECT nextval('ids'); ROLLBACK TO before_definition");
        peer_lock(&second, RelationLockMode::RowExclusive, true);
        peer_lock(&second, RelationLockMode::Share, false);
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn direct_value_calls_release_implicit_locks_and_retain_explicit_read_only_locks() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE SEQUENCE ids CACHE 5");
        assert_eq!(first.nextval("ids").unwrap(), 1);
        peer_lock(&second, RelationLockMode::AccessExclusive, true);
        assert_eq!(first.currval("ids").unwrap(), 1);
        peer_lock(&second, RelationLockMode::AccessExclusive, true);
        assert_eq!(first.lastval().unwrap(), 1);
        peer_lock(&second, RelationLockMode::AccessExclusive, true);
        assert_eq!(first.setval_with_is_called("ids", 42, false).unwrap(), 42);
        peer_lock(&second, RelationLockMode::AccessExclusive, true);
        sql(&first, "BEGIN READ ONLY");
        assert_eq!(first.currval("ids").unwrap(), 1);
        assert_eq!(first.lastval().unwrap(), 1);
        peer_lock(&second, RelationLockMode::Share, false);
        sql(&first, "ROLLBACK");
        peer_lock(&second, RelationLockMode::AccessExclusive, true);
        assert_eq!(first.nextval("ids").unwrap(), 42);
    }
}

#[test]
fn value_workers_use_the_callers_transaction_without_reentering_its_gate() {
    for provider in 0..4 {
        let (_directory, engine) = if provider == 3 {
            (tempfile::tempdir().unwrap(), Engine::new())
        } else {
            let (directory, engine, _peer) = sessions(provider);
            (directory, engine)
        };
        sql(&engine, "CREATE SEQUENCE ids CACHE 5");
        sql(&engine, "BEGIN; CREATE SEQUENCE private_ids CACHE 5");
        std::thread::scope(|scope| {
            let guard = engine.runtime.statement_gate.lock();
            let (send, receive) = std::sync::mpsc::channel();
            let value_engine = &engine;
            let worker = scope.spawn(move || {
                let result = (|| {
                    let mut values = Vec::new();
                    for name in ["ids", "private_ids"] {
                        values.extend([
                            value_engine.nextval_sql(name)?,
                            value_engine.nextval_sql(name)?,
                            value_engine.setval_sql(name, 42, false)?,
                            value_engine.currval_sql(name)?,
                            value_engine.lastval_sql()?,
                            value_engine.nextval_sql(name)?,
                        ]);
                    }
                    Ok::<_, uqa_sql::SQLError>(values)
                })();
                send.send(result).unwrap();
            });
            let result = receive.recv_timeout(std::time::Duration::from_secs(30));
            drop(guard);
            worker.join().unwrap();
            assert_eq!(result.unwrap().unwrap(), [1, 2, 42, 2, 2, 42].repeat(2));
        });
        sql(&engine, "ROLLBACK");
    }
}

#[test]
fn sequence_waits_refresh_definitions_without_advancing_the_query_data_snapshot() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE SEQUENCE ids");
            sql(
                &first,
                "BEGIN; ALTER SEQUENCE ids RESTART 100; INSERT INTO t VALUES(2)",
            );
            let (second, result) = after_wait(&first, second,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT nextval('ids') AS value, (SELECT count(*) FROM t) AS n"),
                "public.ids", "COMMIT");
            let row = result.unwrap().rows.remove(0);
            assert_eq!(row["value"], Value::Int(100), "{provider}/{isolation}");
            assert_eq!(row["n"], Value::Int(1), "{provider}/{isolation}");
            sql(&second, "ROLLBACK");
        }
    }
}
