//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn physical_text_leaf() -> OperatorTree {
    OperatorTree::Term {
        query: "rust search".into(),
        field: Some("body".into()),
        scoring: Some(TextScoringMode::BM25),
        top_k: Some(uqa_operators::TextTopKPlan {
            k: 10,
            strategy: uqa_operators::TextTopKStrategy::Wand,
        }),
    }
}

#[test]
fn physical_text_limit_is_rejected_below_a_parent() {
    assert!(validate_text_top_k_placement(&physical_text_leaf()).is_ok());
    assert!(
        validate_text_top_k_placement(&OperatorTree::Union(vec![physical_text_leaf()])).is_err()
    );
}

fn populate_calibration_fixture(engine: &Engine) {
    engine
        .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    engine
        .sql("CREATE INDEX docs_fts ON docs USING gin (body)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (id, body) VALUES \
             (1, 'rust search engine'), \
             (2, 'rust database query'), \
             (3, 'search ranking calibration')",
            &[],
        )
        .unwrap();
}

fn calibration_then_failure() -> OperatorTree {
    OperatorTree::Composed(vec![
        OperatorTree::Term {
            query: "rust".into(),
            field: Some("body".into()),
            scoring: Some(TextScoringMode::BayesianBM25),
            top_k: None,
        },
        OperatorTree::KNN {
            query_vector: vec![1.0],
            k: 1,
            field: "missing_embedding".into(),
        },
    ])
}

fn assert_failed_tree_rolls_back_calibration(engine: &Engine) {
    assert!(engine.load_scoring_params("docs.body").unwrap().is_none());
    execute_operator_tree(engine, "docs", &[], &calibration_then_failure())
        .expect_err("the malformed downstream vector leaf must fail");
    assert!(
        engine.load_scoring_params("docs.body").unwrap().is_none(),
        "failed operator execution leaked auto-calibration state"
    );
    assert_eq!(engine.transaction_depth(), 0);
}

#[test]
fn failed_calibrating_tree_rolls_back_memory_state() {
    let engine = Engine::new();
    populate_calibration_fixture(&engine);
    assert_failed_tree_rolls_back_calibration(&engine);
}

#[test]
fn failed_calibrating_tree_rolls_back_catalog_and_reopen_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("calibration.sqlite");
    let engine = Engine::open(&path).unwrap();
    populate_calibration_fixture(&engine);
    assert_failed_tree_rolls_back_calibration(&engine);
    drop(engine);

    let reopened = Engine::open(&path).unwrap();
    assert!(reopened.load_scoring_params("docs.body").unwrap().is_none());
}
