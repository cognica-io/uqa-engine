//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regression coverage for SQL text scorer selection.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_engine::{Engine, ScoredEntry, ScoringMode};
use uqa_scoring::{sigmoid, BM25Params, BayesianBM25Params};

fn engine() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, authority TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO docs (id, body, authority) VALUES \
         (1, 'rust rust rust async runtime', 'unknown'), \
         (2, 'rust language guide', 'unknown'), \
         (3, 'python language guide', 'unknown')",
        &[],
    )
    .unwrap();
    eng
}

fn score_map(entries: Vec<ScoredEntry>) -> BTreeMap<i64, f64> {
    entries
        .into_iter()
        .map(|entry| (entry.doc_id as i64, entry.score))
        .collect()
}

fn sql_score_map(eng: &Engine, predicate: &str) -> BTreeMap<i64, f64> {
    let result = eng
        .sql(
            &format!("SELECT id, _score FROM docs WHERE {predicate}"),
            &[],
        )
        .unwrap();
    result
        .rows
        .iter()
        .map(|row| {
            let id = match row.get("id") {
                Some(Value::Int(id)) => *id,
                other => panic!("expected integer id, got {other:?}"),
            };
            let score = match row.get("_score") {
                Some(Value::Float(score)) => *score,
                other => panic!("expected float _score, got {other:?}"),
            };
            (id, score)
        })
        .collect()
}

fn assert_scores_match(got: &BTreeMap<i64, f64>, expected: &BTreeMap<i64, f64>) {
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (id, got_score) in got {
        let expected_score = expected.get(id).expect("same keys");
        assert!(
            (got_score - expected_score).abs() < 1e-12,
            "doc {id}: got {got_score}, expected {expected_score}"
        );
    }
}

#[test]
fn text_match_uses_bm25_scores() {
    let eng = engine();
    let sql = sql_score_map(&eng, "text_match(body, 'rust')");
    let bm25 = score_map(eng.search(
        "docs",
        "body",
        "rust",
        &ScoringMode::BM25(BM25Params::default()),
        usize::MAX,
    ));
    let bayesian = score_map(eng.search(
        "docs",
        "body",
        "rust",
        &ScoringMode::BayesianBM25(BayesianBM25Params::default()),
        usize::MAX,
    ));

    assert_scores_match(&sql, &bm25);
    assert_ne!(bm25, bayesian, "test corpus must distinguish the scorers");
}

#[test]
fn bayesian_match_uses_bayesian_bm25_scores() {
    let eng = engine();
    let sql = sql_score_map(&eng, "bayesian_match(body, 'rust')");
    let bm25 = score_map(eng.search(
        "docs",
        "body",
        "rust",
        &ScoringMode::BM25(BM25Params::default()),
        usize::MAX,
    ));
    let bayesian = score_map(eng.search(
        "docs",
        "body",
        "rust",
        &ScoringMode::BayesianBM25(BayesianBM25Params::default()),
        usize::MAX,
    ));

    assert_scores_match(&sql, &bayesian);
    assert_ne!(bm25, bayesian, "test corpus must distinguish the scorers");
}

#[test]
fn bayesian_match_with_neutral_prior_uses_bayesian_base_scores() {
    let eng = engine();
    let sql = sql_score_map(
        &eng,
        "bayesian_match_with_prior(body, 'rust', authority, 'authority')",
    );
    let bayesian = score_map(eng.search(
        "docs",
        "body",
        "rust",
        &ScoringMode::BayesianBM25(BayesianBM25Params::default()),
        usize::MAX,
    ));

    assert_scores_match(&sql, &bayesian);
}

#[test]
fn staged_retrieval_shorthand_uses_bm25_text_match_scores() {
    let eng = engine();
    let sql = sql_score_map(&eng, "staged_retrieval(body, 'rust', 10)");
    let bm25 = score_map(eng.search(
        "docs",
        "body",
        "rust",
        &ScoringMode::BM25(BM25Params::default()),
        usize::MAX,
    ));

    assert_scores_match(&sql, &bm25);
}

#[test]
fn probabilistic_fusion_rejects_raw_text_match_scores() {
    let eng = engine();
    let err = eng
        .sql(
            "SELECT id FROM docs \
             WHERE fuse_log_odds(text_match(body, 'rust'), bayesian_match(body, 'rust'))",
            &[],
        )
        .unwrap_err()
        .to_string();

    assert!(err.contains("probability-valued"), "{err}");
    assert!(err.contains("text_match"), "{err}");
    assert!(err.contains("bayesian_match"), "{err}");
}

#[test]
fn multi_term_bayesian_calibration_preserves_bm25_ranking() {
    let eng = engine();
    let params = BayesianBM25Params {
        alpha: 1.7,
        beta: 0.8,
        base_rate: 0.08,
        ..BayesianBM25Params::default()
    };
    let bm25 = eng.search(
        "docs",
        "body",
        "rust language",
        &ScoringMode::BM25(params.bm25),
        usize::MAX,
    );
    let bayesian = eng.search(
        "docs",
        "body",
        "rust language",
        &ScoringMode::BayesianBM25(params),
        usize::MAX,
    );

    assert_eq!(
        bm25.iter().map(|entry| entry.doc_id).collect::<Vec<_>>(),
        bayesian
            .iter()
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>()
    );
    for (raw, calibrated) in bm25.iter().zip(&bayesian) {
        // The posterior is Lucene's transform; the corpus prior never
        // enters it (it belongs to fusion).
        let expected = sigmoid(params.alpha * (raw.score - params.beta));
        assert!(
            (calibrated.score - expected).abs() < 1e-12,
            "doc {}: {} != {}",
            raw.doc_id,
            calibrated.score,
            expected
        );
    }
}

#[test]
fn fts_match_calibrates_the_complete_boolean_query_once() {
    let eng = engine();
    let params = BayesianBM25Params {
        alpha: 1.7,
        beta: 0.8,
        base_rate: 0.08,
        ..BayesianBM25Params::default()
    };
    eng.save_scoring_params(
        "docs.body",
        &serde_json::json!({
            "alpha": params.alpha,
            "beta": params.beta,
            "base_rate": params.base_rate,
        })
        .to_string(),
    )
    .unwrap();

    let raw = score_map(eng.search(
        "docs",
        "body",
        "rust language",
        &ScoringMode::BM25(params.bm25),
        usize::MAX,
    ));
    let raw_doc_2 = raw.get(&2).copied().expect("doc 2 matches both terms");
    let expected = BTreeMap::from([(2, sigmoid(params.alpha * (raw_doc_2 - params.beta)))]);
    let sql = sql_score_map(&eng, "fts_match(body, 'rust AND language')");

    assert_scores_match(&sql, &expected);
}

#[test]
fn estimated_parameters_are_persisted_and_used_by_sql_search() {
    let eng = engine();
    let estimated = eng
        .estimate_scoring_params("docs", "body", 4, 2, 42)
        .unwrap();
    let saved = eng.load_scoring_params("docs.body").unwrap();
    let saved: BTreeMap<String, f64> = serde_json::from_str(&saved).unwrap();
    assert_eq!(estimated, saved);

    let params = BayesianBM25Params {
        alpha: estimated["alpha"],
        beta: estimated["beta"],
        base_rate: estimated["base_rate"],
        ..BayesianBM25Params::default()
    };
    let direct = score_map(eng.search(
        "docs",
        "body",
        "rust language",
        &ScoringMode::BayesianBM25(params),
        usize::MAX,
    ));
    let sql = sql_score_map(&eng, "bayesian_match(body, 'rust language')");
    assert_scores_match(&sql, &direct);
}
