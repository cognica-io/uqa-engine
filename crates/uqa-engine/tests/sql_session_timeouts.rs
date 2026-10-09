//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `idle_in_transaction_session_timeout`, `idle_session_timeout` and `transaction_timeout` terminate the session as `PostgreSQL` 18 terminates the backend: the session's transaction is rolled back, which releases its locks, and every later statement reports the termination.

use std::time::{Duration, Instant};

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::{SQLError, SQLResult};

/// A bound well above any limit the tests set, so that a missed termination fails instead of passing late.
const LATE: Duration = Duration::from_secs(20);

fn sql(engine: &Engine, statement: &str) -> SQLResult {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"))
}

fn error(engine: &Engine, statement: &str) -> SQLError {
    match engine.sql(statement, &[]) {
        Ok(_) => panic!("{statement} succeeded"),
        Err(error) => error,
    }
}

/// Wait for the permanent timeout and transaction rollback, or fail once `LATE` has passed. The cancellation signal precedes the idle rollback worker.
fn await_termination(engine: &Engine) -> SQLError {
    let started = Instant::now();
    loop {
        if let Some(termination) = engine
            .session_termination()
            .filter(|_| engine.transaction_depth() == 0)
        {
            return termination;
        }
        assert!(
            started.elapsed() < LATE,
            "session timeout and transaction rollback did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn persistent() -> (tempfile::TempDir, Engine, Engine) {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("database.db")).unwrap();
    let other = engine.new_session().unwrap();
    (directory, engine, other)
}

#[test]
fn a_session_idle_in_a_transaction_is_terminated_and_its_locks_released() {
    let (_directory, idle, other) = persistent();
    sql(
        &idle,
        "CREATE TABLE held (id integer PRIMARY KEY, v integer)",
    );
    sql(&idle, "INSERT INTO held VALUES (1, 1)");
    sql(&idle, "SET idle_in_transaction_session_timeout = 100");
    sql(&idle, "BEGIN");
    sql(&idle, "UPDATE held SET v = 2 WHERE id = 1");
    sql(&other, "SET lock_timeout = '20s'");
    let started = Instant::now();
    sql(&other, "UPDATE held SET v = 3 WHERE id = 1");
    assert!(started.elapsed() < LATE);
    let termination = await_termination(&idle);
    assert_eq!(termination.sqlstate(), Some("25P03"));
    assert_eq!(
        termination.to_string(),
        "terminating connection due to idle-in-transaction timeout"
    );
    for _ in 0..2 {
        let failure = error(&idle, "SELECT 1");
        assert_eq!(failure.sqlstate(), Some("25P03"));
    }
    idle.reset_cancellation();
    assert_eq!(error(&idle, "SELECT 1").sqlstate(), Some("25P03"));
    assert_eq!(
        sql(&other, "SELECT v FROM held WHERE id = 1").rows[0]["v"],
        Value::Int(3)
    );
}

#[test]
fn a_memory_session_idle_in_a_transaction_rolls_back_its_writes() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE kept (id integer PRIMARY KEY)");
    sql(&engine, "SET idle_in_transaction_session_timeout = 50");
    sql(&engine, "BEGIN");
    sql(&engine, "INSERT INTO kept VALUES (1)");
    let termination = await_termination(&engine);
    assert_eq!(termination.sqlstate(), Some("25P03"));
    assert_eq!(engine.transaction_depth(), 0);
}

#[test]
fn an_idle_session_is_terminated_after_idle_session_timeout() {
    let engine = Engine::new();
    sql(&engine, "SET idle_session_timeout = 50");
    let termination = await_termination(&engine);
    assert_eq!(termination.sqlstate(), Some("57P05"));
    assert_eq!(
        error(&engine, "SELECT 1").to_string(),
        "terminating connection due to idle-session timeout"
    );
}

#[test]
fn a_session_that_keeps_working_is_not_idle() {
    let engine = Engine::new();
    sql(&engine, "SET idle_session_timeout = 400");
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(900) {
        sql(&engine, "SELECT 1");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(engine.session_termination().is_none());
    sql(&engine, "RESET idle_session_timeout");
}

#[test]
fn a_statement_that_outlasts_transaction_timeout_terminates_the_session() {
    let engine = Engine::new();
    sql(&engine, "SET transaction_timeout = 100");
    let started = Instant::now();
    let failure = error(&engine, "SELECT pg_sleep(60)");
    assert!(started.elapsed() < LATE);
    assert_eq!(failure.sqlstate(), Some("25P04"));
    assert_eq!(
        failure.to_string(),
        "terminating connection due to transaction timeout"
    );
    assert_eq!(error(&engine, "SELECT 1").sqlstate(), Some("25P04"));
}

#[test]
fn an_idle_transaction_that_outlasts_transaction_timeout_terminates_the_session() {
    let engine = Engine::new();
    // Each statement outside a transaction block is its own transaction, so three statements of 0.4 s pass under a 1 s limit; the margin absorbs the scheduling delays of a loaded test host.
    sql(&engine, "SET transaction_timeout = 1000");
    for _ in 0..3 {
        sql(&engine, "SELECT pg_sleep(0.4)");
    }
    sql(&engine, "BEGIN");
    sql(&engine, "SELECT 1");
    let termination = await_termination(&engine);
    assert_eq!(termination.sqlstate(), Some("25P04"));
}

#[test]
fn no_handler_catches_a_termination() {
    let engine = Engine::new();
    sql(&engine, "SET transaction_timeout = 100");
    let failure = error(
        &engine,
        "DO $$ BEGIN PERFORM pg_sleep(60); EXCEPTION WHEN query_canceled THEN RAISE NOTICE 'canceled'; WHEN OTHERS THEN RAISE NOTICE 'other'; END $$",
    );
    assert_eq!(failure.sqlstate(), Some("25P04"));
    assert_eq!(engine.take_sql_notices().len(), 0);
}

#[test]
fn session_timeouts_have_postgres_settings() {
    let engine = Engine::new();
    let rows = sql(
        &engine,
        "SELECT name, setting, unit, short_desc FROM pg_settings WHERE name IN ('idle_in_transaction_session_timeout', 'idle_session_timeout', 'transaction_timeout') ORDER BY name",
    );
    let described = rows
        .rows
        .iter()
        .map(|row| {
            (
                row["name"].clone(),
                row["setting"].clone(),
                row["unit"].clone(),
                row["short_desc"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        described,
        [
            (
                Value::Str("idle_in_transaction_session_timeout".into()),
                Value::Str("0".into()),
                Value::Str("ms".into()),
                Value::Str(
                    "Sets the maximum allowed idle time between queries, when in a transaction."
                        .into()
                )
            ),
            (
                Value::Str("idle_session_timeout".into()),
                Value::Str("0".into()),
                Value::Str("ms".into()),
                Value::Str(
                    "Sets the maximum allowed idle time between queries, when not in a transaction."
                        .into()
                )
            ),
            (
                Value::Str("transaction_timeout".into()),
                Value::Str("0".into()),
                Value::Str("ms".into()),
                Value::Str(
                    "Sets the maximum allowed duration of any transaction within a session (not a prepared transaction)."
                        .into()
                )
            ),
        ]
    );
    sql(&engine, "SET transaction_timeout = '2min'");
    assert_eq!(
        sql(&engine, "SHOW transaction_timeout").rows[0]["transaction_timeout"],
        Value::Str("2min".into())
    );
}
