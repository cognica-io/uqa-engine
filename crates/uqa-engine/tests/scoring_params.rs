//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-level Bayesian calibration parameter persistence through the save,
//! single-load, and bulk-load APIs.

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn save_then_load_round_trip() {
    let eng = Engine::new();
    eng.save_scoring_params(
        "title.bm25",
        "{\"alpha\":1.2,\"beta\":0.75,\"base_rate\":0.1}",
    )
    .unwrap();
    let got = eng.load_scoring_params("title.bm25").unwrap().unwrap();
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
    let rows = eng.load_all_scoring_params().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "a.signal");
    assert_eq!(rows[1].0, "b.signal");
}

#[test]
fn drop_scoring_params_removes_entry() {
    let eng = Engine::new();
    eng.save_scoring_params("k", "{\"alpha\":1}").unwrap();
    assert!(eng.drop_scoring_params("k").unwrap());
    assert!(eng.load_scoring_params("k").unwrap().is_none());
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
    let got = eng.load_scoring_params("persist.signal").unwrap().unwrap();
    assert!(got.contains("3.14"));
}

#[test]
fn corrupt_persisted_parameters_are_reported() {
    let eng = Engine::new();
    eng.create_default_table("docs", vec!["body".into()])
        .unwrap();
    eng.save_scoring_params("docs.body", "not-json").unwrap();

    let error = eng.bayesian_params_for("docs", "body").unwrap_err();
    assert!(error
        .to_string()
        .contains("decode persisted scoring parameters `docs.body`"));

    let error = eng
        .update_scoring_params("docs", "body", 0.8, 1)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("decode persisted scoring parameters `docs.body`"));
}

#[test]
fn external_parameter_commit_invalidates_execution_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parameter-cache.db");
    let writer = Engine::open(&path).unwrap();
    writer
        .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    writer
        .sql("INSERT INTO docs (id, body) VALUES (1, 'rust search')", &[])
        .unwrap();
    writer
        .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .unwrap();
    writer
        .save_scoring_params("docs.body", r#"{"alpha":1.0,"beta":0.0,"base_rate":0.1}"#)
        .unwrap();
    let reader = Engine::open(&path).unwrap();
    let score = |engine: &Engine| {
        let result = engine
            .sql(
                "SELECT _score FROM docs WHERE bayesian_match(body, 'rust')",
                &[],
            )
            .unwrap();
        match result.rows[0].get("_score") {
            Some(Value::Float(score)) => *score,
            other => panic!("expected floating score, got {other:?}"),
        }
    };
    let before = score(&reader);
    assert_eq!(score(&reader), before);
    writer
        .save_scoring_params("docs.body", r#"{"alpha":1.0,"beta":10.0,"base_rate":0.1}"#)
        .unwrap();
    let after = score(&reader);
    assert!(after < before * 0.01, "before={before}, after={after}");
}
