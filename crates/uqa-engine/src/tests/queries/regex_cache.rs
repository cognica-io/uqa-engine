//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use uqa_core::Value;

const QUERY: &str = "SELECT s ~ '^(a|ab)+$' AS matched FROM (VALUES ('a'), ('abab'), ('b'), (NULL::text)) AS input(s)";

fn verify(engine: &Engine) {
    let result = engine.sql(QUERY, &[]).unwrap();
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["matched"].clone())
            .collect::<Vec<_>>(),
        [
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(false),
            Value::Null
        ]
    );
}

#[test]
fn regex_cache_is_owned_by_the_session_and_survives_transaction_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("regex-cache.db")).unwrap();
    let memory = engine.query_retention_control().unwrap().memory().clone();
    let cache = engine.session_regex_cache();
    assert!(cache.shares_allowance(&memory));
    let peer = engine.new_session().unwrap();
    assert_eq!(memory.used(), 0);
    verify(&engine);
    let retained = cache.reserved_bytes();
    assert!(retained > 0);
    assert_eq!(peer.query_retention_control().unwrap().memory().used(), 0);
    for boundary in ["BEGIN", "ROLLBACK", "BEGIN", "COMMIT"] {
        engine.sql(boundary, &[]).unwrap();
        assert_eq!(cache.reserved_bytes(), retained);
        verify(&engine);
        assert_eq!(cache.reserved_bytes(), retained);
    }
    verify(&peer);
    assert!(peer.query_retention_control().unwrap().memory().used() > 0);
    assert!(!std::sync::Arc::ptr_eq(
        &engine.session_regex_cache(),
        &peer.session_regex_cache()
    ));
    assert!(uqa_sql::expr::current_regex_cache().is_none());
    drop(engine);
    assert_eq!(cache.reserved_bytes(), 0);
    assert_eq!(memory.used(), 0);
    drop(cache);
    assert_eq!(memory.used(), 0);
    verify(&peer);
}

#[test]
fn regex_cache_is_shared_with_portal_workers_and_released_when_the_session_ends() {
    let engine = Engine::new();
    let memory = engine.query_retention_control().unwrap().memory().clone();
    let cache = engine.session_regex_cache();
    verify(&engine);
    let retained = cache.reserved_bytes();
    portal_cycle(&engine);
    assert_eq!(cache.reserved_bytes(), retained);
    verify(&engine);
    assert_eq!(cache.reserved_bytes(), retained);
    drop(engine);
    assert_eq!(cache.reserved_bytes(), 0);
    assert_eq!(memory.used(), 0);
    drop(cache);
    assert_eq!(memory.used(), 0);
}

fn portal_cycle(engine: &Engine) {
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(&format!("DECLARE matches CURSOR FOR {QUERY}"), &[])
        .unwrap();
    let result = engine.sql("FETCH ALL FROM matches", &[]).unwrap();
    assert_eq!(result.rows.len(), 4);
    assert_eq!(result.rows[0]["matched"], Value::Bool(true));
    engine.sql("CLOSE matches; ROLLBACK", &[]).unwrap();
}
