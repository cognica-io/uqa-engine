//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine calibration-report coverage.

use std::collections::BTreeMap;

use uqa_engine::{Engine, ScoringMode};
use uqa_scoring::{sigmoid, BayesianBM25Params, CalibrationMetrics};

fn engine_with_docs() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id SERIAL PRIMARY KEY, content TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX idx_docs_gin ON docs USING gin (content)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (content) VALUES
             ('machine learning algorithms'),
             ('deep learning neural networks'),
             ('database indexing structures'),
             ('search engine optimization')",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn calibration_report_returns_struct() {
    let engine = engine_with_docs();
    let report = engine
        .calibration_report("docs", "content", "learning", &[1, 1, 0, 0])
        .unwrap();
    assert!(report.ece >= 0.0);
    assert!(report.brier >= 0.0);
}

#[test]
fn calibration_report_scores_non_matches_at_raw_bm25_zero() {
    let engine = engine_with_docs();
    let params = BayesianBM25Params {
        alpha: 1.0,
        beta: 2.0,
        ..BayesianBM25Params::default()
    };
    engine
        .save_scoring_params(
            "docs.content",
            &serde_json::json!({
                "alpha": params.alpha,
                "beta": params.beta,
                "base_rate": params.base_rate,
            })
            .to_string(),
        )
        .unwrap();

    let labels = [1, 1, 0, 0];
    let actual = engine
        .calibration_report("docs", "content", "learning", &labels)
        .unwrap();

    let matching_scores: BTreeMap<_, _> = engine
        .search(
            "docs",
            "content",
            "learning",
            &ScoringMode::BayesianBM25(params),
            usize::MAX,
        )
        .unwrap()
        .into_iter()
        .map(|entry| (entry.doc_id, entry.score))
        .collect();
    let non_match_probability = sigmoid(params.alpha * (0.0 - params.beta));
    assert!(non_match_probability > 0.0);
    let probabilities: Vec<_> = (1..=4)
        .map(|doc_id| {
            matching_scores
                .get(&doc_id)
                .copied()
                .unwrap_or(non_match_probability)
        })
        .collect();
    let expected = CalibrationMetrics::report(&probabilities, &labels, 10).unwrap();

    assert_eq!(actual, expected);
}

#[test]
fn calibration_report_wrong_label_count() {
    let engine = engine_with_docs();
    let err = engine
        .calibration_report("docs", "content", "learning", &[1, 0])
        .unwrap_err()
        .to_string();
    assert!(err.contains("labels length"));
}

#[test]
fn calibration_report_nonexistent_table() {
    let engine = engine_with_docs();
    let err = engine
        .calibration_report("nonexistent", "content", "learning", &[1])
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown table") || err.contains("does not exist"));
}
