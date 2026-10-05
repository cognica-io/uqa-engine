//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `statement_timeout`, `lock_timeout` and the `pg_sleep` functions, with the messages, SQLSTATEs and one-shot interrupts of `PostgreSQL` 18.

use std::time::{Duration, Instant};

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::{NoticeLevel, SQLError, SQLResult};

/// A bound well above any limit the tests set, so that a missed interrupt fails instead of passing late.
const LATE: Duration = Duration::from_secs(20);

fn sql(engine: &Engine, statement: &str) -> SQLResult {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"))
}

fn timed_error(engine: &Engine, statement: &str) -> (SQLError, Duration) {
    let started = Instant::now();
    match engine.sql(statement, &[]) {
        Ok(_) => panic!("{statement} succeeded"),
        Err(error) => (error, started.elapsed()),
    }
}

fn persistent() -> (tempfile::TempDir, Engine, Engine) {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("database.db")).unwrap();
    let other = engine.new_session().unwrap();
    (directory, engine, other)
}

#[test]
fn a_statement_that_outlasts_statement_timeout_is_canceled_once() {
    let engine = Engine::new();
    sql(&engine, "SET statement_timeout = 50");
    assert_eq!(
        sql(&engine, "SHOW statement_timeout").rows[0]["statement_timeout"],
        Value::Str("50ms".into())
    );
    let (error, elapsed) = timed_error(&engine, "SELECT pg_sleep(60)");
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(
        error.to_string(),
        "canceling statement due to statement timeout"
    );
    assert!(elapsed < LATE, "{elapsed:?}");
    sql(&engine, "SELECT 1");
    sql(&engine, "SET statement_timeout = '1min'");
    sql(&engine, "SELECT pg_sleep(0.05)");
    sql(&engine, "RESET statement_timeout");
    assert_eq!(
        sql(&engine, "SHOW statement_timeout").rows[0]["statement_timeout"],
        Value::Str("0".into())
    );
}

#[test]
fn each_statement_of_a_query_starts_its_own_statement_timeout() {
    let engine = Engine::new();
    let mut results = Vec::new();
    let outcome = engine.sql_simple_query(
        // Two statements of 0.6 s pass under a 1 s limit that would cancel their sum; the margin absorbs the scheduling delays of a loaded test host.
        "SET statement_timeout = 1000; SELECT pg_sleep(0.6); SELECT pg_sleep(0.6); SELECT pg_sleep(60)",
        &[],
        |result| {
            results.push(result.command_tag.clone());
            Ok(())
        },
    );
    let error = outcome.unwrap_err();
    assert_eq!(
        error.to_string(),
        "canceling statement due to statement timeout"
    );
    assert_eq!(results.len(), 3);
}

#[test]
fn a_timeout_inside_a_transaction_aborts_it() {
    let engine = Engine::new();
    sql(&engine, "BEGIN");
    sql(&engine, "SET LOCAL statement_timeout = 50");
    let (error, _) = timed_error(&engine, "SELECT pg_sleep(60)");
    assert_eq!(error.sqlstate(), Some("57014"));
    let (error, _) = timed_error(&engine, "SELECT 1");
    assert_eq!(error.sqlstate(), Some("25P02"));
    sql(&engine, "ROLLBACK");
    assert_eq!(
        sql(&engine, "SHOW statement_timeout").rows[0]["statement_timeout"],
        Value::Str("0".into())
    );
}

#[test]
fn a_handler_that_names_query_canceled_catches_the_timeout_and_continues() {
    let engine = Engine::new();
    sql(&engine, "SET statement_timeout = 100");
    sql(
        &engine,
        "DO $$ BEGIN PERFORM pg_sleep(60); EXCEPTION WHEN query_canceled THEN RAISE NOTICE 'caught: %', SQLERRM; PERFORM pg_sleep(0.2); RAISE NOTICE 'continued'; END $$",
    );
    let notices = engine
        .take_sql_notices()
        .into_iter()
        .map(|notice| (notice.level, notice.message))
        .collect::<Vec<_>>();
    assert_eq!(
        notices,
        [
            (
                NoticeLevel::Notice,
                "caught: canceling statement due to statement timeout".to_string()
            ),
            (NoticeLevel::Notice, "continued".to_string()),
        ]
    );
    let (error, _) = timed_error(
        &engine,
        "DO $$ BEGIN PERFORM pg_sleep(60); EXCEPTION WHEN OTHERS THEN RAISE NOTICE 'caught'; END $$",
    );
    assert_eq!(error.sqlstate(), Some("57014"));
    assert!(engine.take_sql_notices().is_empty());
}

#[test]
fn a_cancel_from_another_thread_ends_a_sleep_at_once() {
    let engine = std::sync::Arc::new(Engine::new());
    let canceler = std::sync::Arc::clone(&engine);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        canceler.cancel();
    });
    let (error, elapsed) = timed_error(&engine, "SELECT pg_sleep(60)");
    handle.join().unwrap();
    assert_eq!(error.to_string(), "canceling statement due to user request");
    assert!(elapsed < LATE, "{elapsed:?}");
    engine.reset_cancellation();
}

#[test]
fn sleep_functions_return_void_after_their_time() {
    let engine = Engine::new();
    let result = sql(
        &engine,
        "SELECT pg_sleep(-1) AS negative, pg_sleep(NULL) AS absent, pg_sleep('NaN') AS undefined, pg_sleep(0) AS zero",
    );
    assert_eq!(result.rows[0]["absent"], Value::Null);
    let started = Instant::now();
    sql(&engine, "SELECT pg_sleep(0.05)");
    sql(&engine, "SELECT pg_sleep_for('50 milliseconds')");
    sql(
        &engine,
        "SELECT pg_sleep_until(clock_timestamp() + interval '50 milliseconds')",
    );
    assert!(started.elapsed() >= Duration::from_millis(150));
    let rows = sql(
        &engine,
        "SELECT p.oid::integer AS oid, proname::text AS name, provolatile::text AS volatile, proparallel::text AS parallel, prolang::integer AS lang FROM pg_proc AS p WHERE proname::text LIKE 'pg_sleep%' ORDER BY p.oid",
    );
    let described = rows
        .rows
        .iter()
        .map(|row| {
            (
                row["oid"].clone(),
                row["name"].clone(),
                row["volatile"].clone(),
                row["parallel"].clone(),
                row["lang"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        described,
        [
            (
                Value::Int(2626),
                Value::Str("pg_sleep".into()),
                Value::Str("v".into()),
                Value::Str("s".into()),
                Value::Int(12)
            ),
            (
                Value::Int(3935),
                Value::Str("pg_sleep_for".into()),
                Value::Str("v".into()),
                Value::Str("s".into()),
                Value::Int(14)
            ),
            (
                Value::Int(3936),
                Value::Str("pg_sleep_until".into()),
                Value::Str("v".into()),
                Value::Str("s".into()),
                Value::Int(14)
            ),
        ]
    );
}

#[test]
fn a_relation_lock_wait_that_outlasts_lock_timeout_reports_55p03() {
    let (_directory, holder, waiter) = persistent();
    sql(
        &holder,
        "CREATE TABLE locked (id integer PRIMARY KEY, v integer)",
    );
    sql(&holder, "INSERT INTO locked VALUES (1, 1)");
    sql(&holder, "BEGIN");
    sql(&holder, "LOCK TABLE locked IN ACCESS EXCLUSIVE MODE");
    sql(&waiter, "SET lock_timeout = '100ms'");
    let (error, elapsed) = timed_error(&waiter, "SELECT * FROM locked");
    assert_eq!(error.sqlstate(), Some("55P03"));
    assert_eq!(error.to_string(), "canceling statement due to lock timeout");
    assert!(elapsed >= Duration::from_millis(100), "{elapsed:?}");
    assert!(elapsed < LATE, "{elapsed:?}");
    sql(&holder, "COMMIT");
    sql(&waiter, "SELECT * FROM locked");
}

#[test]
fn a_row_lock_wait_that_outlasts_lock_timeout_can_be_caught() {
    let (_directory, holder, waiter) = persistent();
    sql(
        &holder,
        "CREATE TABLE rows_locked (id integer PRIMARY KEY, v integer)",
    );
    sql(&holder, "INSERT INTO rows_locked VALUES (1, 1)");
    sql(&holder, "BEGIN");
    sql(&holder, "SELECT * FROM rows_locked WHERE id = 1 FOR UPDATE");
    sql(&waiter, "SET lock_timeout = 100");
    let (error, elapsed) = timed_error(&waiter, "UPDATE rows_locked SET v = 2 WHERE id = 1");
    assert_eq!(error.sqlstate(), Some("55P03"));
    assert_eq!(error.to_string(), "canceling statement due to lock timeout");
    assert!(elapsed < LATE, "{elapsed:?}");
    sql(
        &waiter,
        "DO $$ BEGIN UPDATE rows_locked SET v = 3 WHERE id = 1; EXCEPTION WHEN lock_not_available THEN RAISE NOTICE 'not available: %', SQLERRM; END $$",
    );
    let notices = waiter.take_sql_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].message,
        "not available: canceling statement due to lock timeout"
    );
    sql(&holder, "COMMIT");
    sql(&waiter, "UPDATE rows_locked SET v = 4 WHERE id = 1");
}

#[test]
fn statement_timeout_also_ends_a_lock_wait() {
    let (_directory, holder, waiter) = persistent();
    sql(&holder, "CREATE TABLE waited (id integer PRIMARY KEY)");
    sql(&holder, "BEGIN");
    sql(&holder, "LOCK TABLE waited IN ACCESS EXCLUSIVE MODE");
    sql(&waiter, "SET statement_timeout = 100");
    let (error, elapsed) = timed_error(&waiter, "SELECT * FROM waited");
    assert_eq!(
        error.to_string(),
        "canceling statement due to statement timeout"
    );
    assert!(elapsed < LATE, "{elapsed:?}");
    sql(&holder, "ROLLBACK");
}
