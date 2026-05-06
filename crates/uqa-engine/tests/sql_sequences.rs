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
