//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `Engine::cancel` propagating through the SQL surface.

use std::thread;
use std::time::Duration;

use uqa_core::QueryCancelled;
use uqa_engine::Engine;
use uqa_sql::SQLError;

#[test]
fn cancel_before_execute_aborts_query() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.cancel();
    let err = eng.sql("SELECT body FROM t", &[]).unwrap_err();
    assert!(
        matches!(err, SQLError::Cancelled(QueryCancelled)),
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
