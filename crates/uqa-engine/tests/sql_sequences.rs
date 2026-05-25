//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `CREATE SEQUENCE` / `ALTER SEQUENCE` + `nextval` / `currval` /
//! `setval` round-trips.

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn sequence_create_and_nextval_via_sql() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE myseq START 1", &[]).unwrap();
    let first = eng.sql("SELECT nextval('myseq') AS v", &[]).unwrap();
    assert_eq!(first.rows[0]["v"], Value::Int(1));
    let second = eng.sql("SELECT nextval('myseq') AS v", &[]).unwrap();
    assert_eq!(second.rows[0]["v"], Value::Int(2));
}

#[test]
fn sequence_currval_via_sql() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s2 START 10", &[]).unwrap();
    eng.sql("SELECT nextval('s2') AS v", &[]).unwrap();
    let result = eng.sql("SELECT currval('s2') AS v", &[]).unwrap();
    assert_eq!(result.rows[0]["v"], Value::Int(10));
}

#[test]
fn sequence_setval_via_sql_updates_currval() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s3 START 1", &[]).unwrap();
    eng.sql("SELECT nextval('s3') AS v", &[]).unwrap();
    eng.sql("SELECT setval('s3', 100) AS v", &[]).unwrap();
    let result = eng.sql("SELECT currval('s3') AS v", &[]).unwrap();
    assert_eq!(result.rows[0]["v"], Value::Int(100));
}

#[test]
fn sequence_increment_via_sql() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s4 START 1 INCREMENT 5", &[])
        .unwrap();
    let first = eng.sql("SELECT nextval('s4') AS v", &[]).unwrap();
    assert_eq!(first.rows[0]["v"], Value::Int(1));
    let second = eng.sql("SELECT nextval('s4') AS v", &[]).unwrap();
    assert_eq!(second.rows[0]["v"], Value::Int(6));
}

#[test]
fn create_sequence_default_start_increment_one() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s1", &[]).unwrap();
    assert_eq!(eng.nextval("s1").unwrap(), 1);
    assert_eq!(eng.nextval("s1").unwrap(), 2);
    assert_eq!(eng.currval("s1").unwrap(), 2);
}

#[test]
fn create_sequence_with_options() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s2 START 10 INCREMENT 5", &[])
        .unwrap();
    assert_eq!(eng.nextval("s2").unwrap(), 10);
    assert_eq!(eng.nextval("s2").unwrap(), 15);
}

#[test]
fn alter_sequence_restart_resets_current() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s3 START 100", &[]).unwrap();
    let _ = eng.nextval("s3").unwrap();
    let _ = eng.nextval("s3").unwrap();
    eng.sql("ALTER SEQUENCE s3 RESTART WITH 50", &[]).unwrap();
    assert_eq!(eng.nextval("s3").unwrap(), 50);
}

#[test]
fn nextval_through_select_projection() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s4", &[]).unwrap();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1), (2), (3)", &[])
        .unwrap();
    let result = eng.sql("SELECT nextval('s4') AS n FROM t", &[]).unwrap();
    let ns: Vec<i64> = result
        .rows
        .iter()
        .map(|r| match r.get("n") {
            Some(Value::Int(n)) => *n,
            _ => -1,
        })
        .collect();
    assert_eq!(ns, vec![1, 2, 3]);
}

#[test]
fn setval_overrides_current() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s5", &[]).unwrap();
    let _ = eng.nextval("s5").unwrap();
    let _ = eng.setval("s5", 100).unwrap();
    assert_eq!(eng.nextval("s5").unwrap(), 101);
}

#[test]
fn sequence_state_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sequences.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql("CREATE SCHEMA app", &[]).unwrap();
        eng.sql("SET search_path TO app, public", &[]).unwrap();
        eng.sql("CREATE SEQUENCE app.s START 10 INCREMENT 5", &[])
            .unwrap();
        assert_eq!(
            eng.sql("SELECT nextval('s') AS v", &[]).unwrap().rows[0]["v"],
            Value::Int(10)
        );
    }
    let eng = Engine::open(&db).unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    assert_eq!(
        eng.sql("SELECT nextval('s') AS v", &[]).unwrap().rows[0]["v"],
        Value::Int(15)
    );
}
