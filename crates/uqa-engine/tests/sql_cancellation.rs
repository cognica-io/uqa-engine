//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `Engine::cancel` propagating through the SQL surface.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use uqa_core::{CancellationReason, QueryCancelled, Value};
use uqa_engine::{Engine, SQLFunctionOptions, SQLFunctionVolatility};
use uqa_sql::{expr::check_statement_cancellation, SQLError};

#[test]
fn cancel_before_execute_aborts_query() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.cancel();
    let err = eng.sql("SELECT body FROM t", &[]).unwrap_err();
    assert!(
        matches!(err, SQLError::Cancelled(QueryCancelled::USER_REQUEST)),
        "expected SQLError::Cancelled, got {err:?}"
    );
    assert_eq!(err.sqlstate(), Some("57014"));
}

#[test]
fn reset_cancellation_lets_next_query_run() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (body) VALUES ('a')", &[]).unwrap();
    eng.cancel();
    assert!(eng.sql("SELECT body FROM t", &[]).is_err());
    eng.reset_cancellation();
    let res = eng.sql("SELECT body FROM t", &[]).unwrap();
    assert_eq!(res.rows.len(), 1);
}

#[test]
fn cancel_from_other_thread_visible_to_engine() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    let token = eng.cancellation_token();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(5));
        token.cancel();
    });
    // Spin until the cancellation lands; bounded so a regression
    // doesn't hang the test indefinitely.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !eng.is_cancelled() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert!(eng.is_cancelled(), "cancel signal never propagated");
    let err = eng.sql("SELECT body FROM t", &[]).unwrap_err();
    assert!(matches!(err, SQLError::Cancelled(_)));
}

type Observations = Arc<Mutex<Vec<Result<(), QueryCancelled>>>>;

/// Register `cancel_and_observe()`, which cancels its session through `Engine::cancel` and records the statement cancellation `check_statement_cancellation` then reports, as work inside one scalar evaluation observes a cancel request without receiving its statement's controls.
fn register_cancel_and_observe(engine: &Arc<Engine>) -> Observations {
    let observations = Observations::default();
    let recorded = Arc::clone(&observations);
    let session = Arc::downgrade(engine);
    engine
        .register_scalar_function("cancel_and_observe", move |_: &[Value]| {
            session
                .upgrade()
                .expect("the session outlives its statements")
                .cancel();
            let observed = check_statement_cancellation();
            recorded.lock().unwrap().push(observed);
            observed?;
            Ok(Value::Bool(false))
        })
        .unwrap();
    observations
}

/// The evaluation observed the cancel request, and the statement or API call failed with it.
fn assert_observed_cancel_request<T: std::fmt::Debug>(
    observations: &Observations,
    result: Result<T, SQLError>,
) {
    assert_eq!(
        std::mem::take(&mut *observations.lock().unwrap()),
        [Err(QueryCancelled::USER_REQUEST)]
    );
    let error = result.unwrap_err();
    assert!(
        matches!(
            error.primary(),
            SQLError::Cancelled(QueryCancelled::USER_REQUEST)
        ),
        "expected the observed cancellation, got {error:?}"
    );
    assert_eq!(error.sqlstate(), Some("57014"));
}

#[test]
fn statement_evaluation_observes_engine_cancel_through_the_statement_scope() {
    let engine = Arc::new(Engine::new());
    let observations = register_cancel_and_observe(&engine);
    assert_observed_cancel_request(
        &observations,
        engine.sql("SELECT cancel_and_observe() AS value", &[]),
    );
    engine.reset_cancellation();
    engine
        .sql(
            "CREATE FUNCTION observe_in_body() RETURNS boolean LANGUAGE sql AS 'SELECT cancel_and_observe()'",
            &[],
        )
        .unwrap();
    assert_observed_cancel_request(
        &observations,
        engine.sql("SELECT observe_in_body() AS value", &[]),
    );
    engine.reset_cancellation();
    engine
        .sql(
            "PREPARE observe AS SELECT cancel_and_observe() AS value",
            &[],
        )
        .unwrap();
    assert!(observations.lock().unwrap().is_empty());
    assert_observed_cancel_request(&observations, engine.sql("EXECUTE observe", &[]));
    engine.reset_cancellation();
    assert_eq!(engine.sql("SELECT 1 AS value", &[]).unwrap().rows.len(), 1);
    assert!(observations.lock().unwrap().is_empty());
}

#[test]
fn portal_worker_evaluation_observes_engine_cancel() {
    let engine = Arc::new(Engine::new());
    let observations = register_cancel_and_observe(&engine);
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE observed CURSOR FOR SELECT cancel_and_observe() AS value FROM generate_series(1, 3) AS g(i)",
            &[],
        )
        .unwrap();
    assert!(observations.lock().unwrap().is_empty());
    assert_observed_cancel_request(&observations, engine.sql("FETCH NEXT FROM observed", &[]));
    engine.reset_cancellation();
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn scroll_cursor_branch_evaluation_observes_engine_cancel() {
    let engine = Arc::new(Engine::new());
    let observations = register_cancel_and_observe(&engine);
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE observed SCROLL CURSOR FOR SELECT i FROM generate_series(1, 2) AS g(i) WHERE cancel_and_observe() IS NOT NULL UNION ALL SELECT i FROM generate_series(3, 4) AS g(i)",
            &[],
        )
        .unwrap();
    assert!(observations.lock().unwrap().is_empty());
    assert_observed_cancel_request(&observations, engine.sql("FETCH NEXT FROM observed", &[]));
    engine.reset_cancellation();
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn statement_timeout_interrupts_a_long_scalar_evaluation() {
    let engine = Engine::new();
    engine
        .register_scalar_function("interruptible_wait", |_: &[Value]| {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(60) {
                check_statement_cancellation()?;
                thread::sleep(Duration::from_millis(1));
            }
            Err(SQLError::Internal(
                "the statement timeout never reached the evaluation".into(),
            ))
        })
        .unwrap();
    engine.sql("SET statement_timeout = 50", &[]).unwrap();
    let started = Instant::now();
    let error = engine
        .sql("SELECT interruptible_wait() AS value", &[])
        .unwrap_err();
    assert!(
        matches!(&error, SQLError::Cancelled(cancelled) if cancelled.reason == CancellationReason::StatementTimeout),
        "expected the statement timeout, got {error:?}"
    );
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(
        error.to_string(),
        "canceling statement due to statement timeout"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(engine.sql("SELECT 1 AS value", &[]).unwrap().rows.len(), 1);
}

/// Register `cancel_and_observe_key(text)`, a host function that a stored generated column reaches through an immutable SQL function whenever a direct API writes a row. Once `armed`, it cancels its session through `Engine::cancel`, records the statement cancellation `check_statement_cancellation` then reports and the transaction clock the evaluation reads, and fails with the cancellation.
fn register_armed_generation_probe(
    engine: &Arc<Engine>,
    armed: &Arc<AtomicBool>,
    clocks: &Arc<Mutex<Vec<Option<i64>>>>,
) -> Observations {
    let observations = Observations::default();
    let recorded = Arc::clone(&observations);
    let clocks = Arc::clone(clocks);
    let armed = Arc::clone(armed);
    let session = Arc::downgrade(engine);
    engine
        .register_scalar_function_with_options(
            "cancel_and_observe_key",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable),
            move |args: &[Value]| {
                if !armed.load(Ordering::Acquire) {
                    return Ok(args.first().cloned().unwrap_or(Value::Null));
                }
                session
                    .upgrade()
                    .expect("the session outlives its API calls")
                    .cancel();
                clocks
                    .lock()
                    .unwrap()
                    .push(uqa_sql::expr::transaction_clock_micros());
                let observed = check_statement_cancellation();
                recorded.lock().unwrap().push(observed);
                observed?;
                Ok(Value::Null)
            },
        )
        .unwrap();
    observations
}

#[test]
fn direct_row_writes_observe_engine_cancel_while_generating_columns() {
    let engine = Arc::new(Engine::new());
    let armed = Arc::new(AtomicBool::new(false));
    let clocks = Arc::new(Mutex::new(Vec::new()));
    let observations = register_armed_generation_probe(&engine, &armed, &clocks);
    engine
        .sql(
            "CREATE FUNCTION generated_key(input text) RETURNS text IMMUTABLE LANGUAGE sql AS 'SELECT cancel_and_observe_key(input)::text'; CREATE TABLE direct_docs (id INTEGER PRIMARY KEY, body TEXT, generated TEXT GENERATED ALWAYS AS (generated_key(body)) STORED); INSERT INTO direct_docs (id, body) VALUES (1, 'stored')",
            &[],
        )
        .unwrap();
    let stored = engine
        .find_doc_id_by_field("direct_docs", "id", &Value::Int(1))
        .unwrap()
        .expect("the stored row");
    let added = BTreeMap::from([
        ("id".to_string(), Value::Int(2)),
        ("body".to_string(), Value::Str("added".into())),
    ]);
    let updates = BTreeMap::from([("body".to_string(), Value::Str("updated".into()))]);
    armed.store(true, Ordering::Release);

    assert_observed_cancel_request(
        &observations,
        engine.add_document("direct_docs", 2, added.clone()),
    );
    engine.reset_cancellation();
    assert_observed_cancel_request(
        &observations,
        engine.update_document_fields("direct_docs", stored, updates.clone(), BTreeMap::new()),
    );
    engine.reset_cancellation();
    assert_observed_cancel_request(
        &observations,
        engine.patch_document_fields("direct_docs", stored, &updates, &BTreeMap::new()),
    );
    engine.reset_cancellation();
    // Outside any transaction a memory engine has no transaction clock, so the evaluation reads the wall clock as `now()` does.
    assert_eq!(std::mem::take(&mut *clocks.lock().unwrap()), [None; 3]);

    engine.begin().unwrap();
    assert_observed_cancel_request(&observations, engine.add_document("direct_docs", 2, added));
    engine.reset_cancellation();
    engine.rollback().unwrap();
    // Inside a transaction the evaluation reads the transaction's start.
    let clocks = std::mem::take(&mut *clocks.lock().unwrap());
    assert!(matches!(clocks.as_slice(), [Some(_)]), "{clocks:?}");

    armed.store(false, Ordering::Release);
    let rows = engine
        .sql(
            "SELECT id, body, generated FROM direct_docs ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0]["body"], Value::Str("stored".into()));
    assert_eq!(rows.rows[0]["generated"], Value::Str("stored".into()));
}
