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
    eng.sql(
        "CREATE TABLE doc_meta (doc_id INTEGER PRIMARY KEY, category TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO doc_meta (doc_id, category) VALUES \
         (1, 'systems'), (2, 'language'), (3, 'language')",
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

fn projected_score_map(eng: &Engine, sql: &str) -> BTreeMap<i64, f64> {
    let result = eng.sql(sql, &[]).unwrap();
    assert!(
        result
            .rows
            .iter()
            .all(|row| row.keys().all(|column| !column.contains('\0'))),
        "internal score provenance leaked into SQL output: {:?}",
        result.rows
    );
    result
        .rows
        .iter()
        .map(|row| {
            let id = match row.get("id") {
                Some(Value::Int(id)) => *id,
                other => panic!("expected integer id, got {other:?}"),
            };
            let score = match row.get("score") {
                Some(Value::Float(score)) => *score,
                other => panic!("expected float score, got {other:?}"),
            };
            (id, score)
        })
        .collect()
}

#[test]
fn text_match_uses_bm25_scores() {
    let eng = engine();
    let sql = sql_score_map(&eng, "text_match(body, 'rust')");
    let bm25 = score_map(
        eng.search(
            "docs",
            "body",
            "rust",
            &ScoringMode::BM25(BM25Params::default()),
            usize::MAX,
        )
        .unwrap(),
    );
    let bayesian = score_map(
        eng.search(
            "docs",
            "body",
            "rust",
            &ScoringMode::BayesianBM25(BayesianBM25Params::default()),
            usize::MAX,
        )
        .unwrap(),
    );

    assert_scores_match(&sql, &bm25);
    assert_ne!(bm25, bayesian, "test corpus must distinguish the scorers");
}

#[test]
fn bayesian_match_uses_bayesian_bm25_scores() {
    let eng = engine();
    let sql = sql_score_map(&eng, "bayesian_match(body, 'rust')");
    let bm25 = score_map(
        eng.search(
            "docs",
            "body",
            "rust",
            &ScoringMode::BM25(BM25Params::default()),
            usize::MAX,
        )
        .unwrap(),
    );
    let bayesian = score_map(
        eng.search(
            "docs",
            "body",
            "rust",
            &ScoringMode::BayesianBM25(BayesianBM25Params::default()),
            usize::MAX,
        )
        .unwrap(),
    );

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
    let bayesian = score_map(
        eng.search(
            "docs",
            "body",
            "rust",
            &ScoringMode::BayesianBM25(BayesianBM25Params::default()),
            usize::MAX,
        )
        .unwrap(),
    );

    assert_scores_match(&sql, &bayesian);
}

#[test]
fn staged_retrieval_shorthand_uses_bm25_text_match_scores() {
    let eng = engine();
    let sql = sql_score_map(&eng, "staged_retrieval(body, 'rust', 10)");
    let bm25 = score_map(
        eng.search(
            "docs",
            "body",
            "rust",
            &ScoringMode::BM25(BM25Params::default()),
            usize::MAX,
        )
        .unwrap(),
    );

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
fn score_projection_requires_a_score_bearing_retrieval_context() {
    let eng = engine();
    for sql in [
        "SELECT score_bm25(body, 'rust') FROM docs",
        "SELECT score_bm25(body, 'rust') FROM docs WHERE id = 1",
        "SELECT score_bayesian_bm25(body, 'rust') FROM docs WHERE body IS NOT NULL",
    ] {
        let error = eng
            .sql(sql, &[])
            .expect_err("an unexecuted scorer must not be represented as a zero score");
        assert!(error.to_string().contains("score-bearing"), "{error}");
    }
}

#[test]
fn score_projection_accepts_executed_retrieval_and_hybrid_rows() {
    let eng = engine();
    let bm25 = sql_score_map(&eng, "text_match(body, 'rust')");
    let projected = projected_score_map(
        &eng,
        "SELECT id, score_bm25(body, 'rust') AS score \
         FROM docs WHERE text_match(body, 'rust') ORDER BY id",
    );
    assert_scores_match(&projected, &bm25);

    let hybrid = projected_score_map(
        &eng,
        "SELECT id, score_bm25(body, 'rust') AS score \
         FROM docs WHERE text_match(body, 'rust') AND id = 1",
    );
    assert_eq!(hybrid.len(), 1);
    assert_eq!(hybrid.get(&1), bm25.get(&1));

    let bayesian = sql_score_map(&eng, "bayesian_match(body, 'rust')");
    let projected_bayesian = projected_score_map(
        &eng,
        "SELECT id, score_bayesian_bm25(body, 'rust') AS score \
         FROM docs WHERE bayesian_match(body, 'rust') ORDER BY id",
    );
    assert_scores_match(&projected_bayesian, &bayesian);
}

#[test]
fn score_provenance_survives_derived_tables_and_joins() {
    let eng = engine();
    let expected = sql_score_map(&eng, "text_match(body, 'rust')");
    let derived = projected_score_map(
        &eng,
        "SELECT hit.id AS id, score_bm25(hit.body, 'rust') AS score \
         FROM (SELECT id, body FROM docs WHERE text_match(body, 'rust')) AS hit \
         ORDER BY hit.id",
    );
    assert_scores_match(&derived, &expected);

    let joined = projected_score_map(
        &eng,
        "SELECT d.id AS id, score_bm25(d.body, 'rust') AS score \
         FROM docs AS d JOIN doc_meta AS m ON d.id = m.doc_id \
         WHERE text_match(d.body, 'rust') ORDER BY d.id",
    );
    assert_scores_match(&joined, &expected);
}

#[test]
fn hidden_score_provenance_does_not_change_distinct_semantics() {
    let eng = engine();
    let result = eng
        .sql(
            "SELECT DISTINCT 'same' AS marker \
             FROM docs WHERE text_match(body, 'rust')",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(
        result.rows[0].get("marker"),
        Some(&Value::Str("same".into()))
    );
    assert!(result.rows[0].keys().all(|column| !column.contains('\0')));
}

#[test]
fn fusion_keeps_declared_signal_count_when_one_signal_is_empty() {
    let eng = engine();
    let with_empty = sql_score_map(
        &eng,
        "fuse_log_odds(bayesian_match(body, 'rust'), bayesian_match(body, 'zzzabsent'))",
    );
    let with_unrelated = sql_score_map(
        &eng,
        "fuse_log_odds(bayesian_match(body, 'rust'), bayesian_match(body, 'python'))",
    );

    // Documents 1 and 2 match only the first signal in both queries;
    // their fused scores must not depend on whether the second signal
    // matched elsewhere, because the declared signal count governs the
    // fusion either way (Lucene PR 16410 semantics).
    for id in [1, 2] {
        let empty_score = with_empty.get(&id).expect("doc matches the first signal");
        let unrelated_score = with_unrelated
            .get(&id)
            .expect("doc matches the first signal");
        assert!(
            (empty_score - unrelated_score).abs() < 1e-12,
            "doc {id}: {empty_score} vs {unrelated_score}"
        );
    }
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
    let bm25 = eng
        .search(
            "docs",
            "body",
            "rust language",
            &ScoringMode::BM25(params.bm25),
            usize::MAX,
        )
        .unwrap();
    let bayesian = eng
        .search(
            "docs",
            "body",
            "rust language",
            &ScoringMode::BayesianBM25(params),
            usize::MAX,
        )
        .unwrap();

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

    let raw = score_map(
        eng.search(
            "docs",
            "body",
            "rust language",
            &ScoringMode::BM25(params.bm25),
            usize::MAX,
        )
        .unwrap(),
    );
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
    let saved = eng.load_scoring_params("docs.body").unwrap().unwrap();
    let saved: BTreeMap<String, f64> = serde_json::from_str(&saved).unwrap();
    assert_eq!(estimated, saved);

    let params = BayesianBM25Params {
        alpha: estimated["alpha"],
        beta: estimated["beta"],
        base_rate: estimated["base_rate"],
        ..BayesianBM25Params::default()
    };
    let direct = score_map(
        eng.search(
            "docs",
            "body",
            "rust language",
            &ScoringMode::BayesianBM25(params),
            usize::MAX,
        )
        .unwrap(),
    );
    let sql = sql_score_map(&eng, "bayesian_match(body, 'rust language')");
    assert_scores_match(&sql, &direct);
}
