//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-level transaction lifecycle convenience methods. Mirrors
//! Python's `Engine.begin/commit/rollback/savepoint`.

use uqa_engine::Engine;

#[test]
fn begin_commit_round_trip() {
    let eng = Engine::new();
    assert_eq!(eng.transaction_depth(), 0);
    eng.begin().unwrap();
    assert_eq!(eng.transaction_depth(), 1);
    eng.commit().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn nested_begin_commit_pops_one_frame_at_a_time() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.begin().unwrap();
    assert_eq!(eng.transaction_depth(), 2);
    eng.commit().unwrap();
    assert_eq!(eng.transaction_depth(), 1);
    eng.rollback().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn savepoint_release_round_trip() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.savepoint("sp1").unwrap();
    eng.release_savepoint("sp1").unwrap();
    eng.commit().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn rollback_to_savepoint_keeps_frame_open() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.savepoint("sp1").unwrap();
    eng.rollback_to_savepoint("sp1").unwrap();
    assert_eq!(eng.transaction_depth(), 1);
    eng.rollback().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn close_drops_open_transactions() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.begin().unwrap();
    assert_eq!(eng.transaction_depth(), 2);
    eng.close();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn commit_without_begin_errors() {
    let eng = Engine::new();
    assert!(eng.commit().is_err());
}
