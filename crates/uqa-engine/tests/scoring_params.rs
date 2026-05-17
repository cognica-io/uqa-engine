//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-level Bayesian calibration parameter persistence.
//! Mirrors the canonical UQA implementation's `Engine.save_scoring_params /
//! load_scoring_params / load_all_scoring_params`.

use uqa_engine::Engine;

#[test]
fn save_then_load_round_trip() {
    let eng = Engine::new();
    eng.save_scoring_params(
        "title.bm25",
        "{\"alpha\":1.2,\"beta\":0.75,\"base_rate\":0.1}",
    )
    .unwrap();
    let got = eng.load_scoring_params("title.bm25").unwrap();
    assert!(got.contains("\"alpha\":1.2"));
    assert!(got.contains("\"base_rate\":0.1"));
}

#[test]
fn load_all_returns_sorted_pairs() {
    let eng = Engine::new();
    eng.save_scoring_params("b.signal", "{\"alpha\":2}")
        .unwrap();
    eng.save_scoring_params("a.signal", "{\"alpha\":1}")
        .unwrap();
    let rows = eng.load_all_scoring_params();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "a.signal");
    assert_eq!(rows[1].0, "b.signal");
}

#[test]
fn drop_scoring_params_removes_entry() {
    let eng = Engine::new();
    eng.save_scoring_params("k", "{\"alpha\":1}").unwrap();
    assert!(eng.drop_scoring_params("k"));
    assert!(eng.load_scoring_params("k").is_none());
}

#[test]
fn round_trip_through_sqlite_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine.db");
    {
        let eng = Engine::open(&path).unwrap();
        eng.save_scoring_params("persist.signal", "{\"alpha\":3.14}")
            .unwrap();
    }
    let eng = Engine::open(&path).unwrap();
    let got = eng.load_scoring_params("persist.signal").unwrap();
    assert!(got.contains("3.14"));
}
