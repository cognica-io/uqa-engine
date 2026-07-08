//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Every lowering of `multi_field_match` must produce the same scores:
//! the operator-tree pipeline (bare and mixed-predicate shapes) and the
//! legacy dispatch fallback all delegate to one implementation, so a
//! pass-all ordinary predicate can never change how a match ranks.
//! Previously the operator-tree path padded unmatched fields with 0.5
//! instead of the calibrated no-match prior, which re-introduced the
//! small-corpus field-weight inversion for mixed-predicate queries.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLParam;

fn fusion_param() -> Vec<SQLParam> {
    vec![SQLParam::scalar(Value::Str("fusion".into()))]
}

fn engine_with_small_corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE pages (id INTEGER PRIMARY KEY, status TEXT, title TEXT, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX pages_text ON pages USING gin (title, body)",
        &[],
    )
    .unwrap();
    for index in 0..6 {
        eng.sql(
            &format!(
                "INSERT INTO pages (id, status, title, body) VALUES \
                 ({}, 'accepted', 'grocery list {index}', \
                  'apples rice and beans for week {index} with notes and reminders \
                   about the market trip and pantry stock levels')",
                100 + index
            ),
            &[],
        )
        .unwrap();
    }
    eng.sql(
        "INSERT INTO pages (id, status, title, body) VALUES \
         (1, 'accepted', 'fusion scoring', \
          'fusion ranking covered in depth with examples and calibration walkthroughs \
           for the retrieval pipeline'), \
         (2, 'accepted', 'daily journal', \
          'one fusion mention only in passing today among other unrelated notes about \
           weather meals and errands')",
        &[],
    )
    .unwrap();
    eng
}

fn scored_ids(result: &uqa_sql::SQLResult) -> Vec<(i64, f64)> {
    result
        .rows
        .iter()
        .map(|row| {
            let id = match row.get("id") {
                Some(Value::Int(value)) => *value,
                other => panic!("expected Int id, got {other:?}"),
            };
            let score = match row.get("_score") {
                Some(Value::Float(value)) => *value,
                other => panic!("expected Float score, got {other:?}"),
            };
            (id, score)
        })
        .collect()
}

#[test]
fn every_lowering_of_multi_field_match_scores_identically() {
    let eng = engine_with_small_corpus();

    // Operator-tree pipeline, bare match shape.
    let bare = eng
        .sql(
            "SELECT id, _score FROM pages \
              WHERE multi_field_match(title, body, $1, 2.0, 1.0) \
              ORDER BY _score DESC",
            &fusion_param(),
        )
        .unwrap();

    // Operator-tree pipeline, mixed shape: an ordinary predicate that
    // filters nothing plus the same match (maek's wiki search shape).
    let mixed = eng
        .sql(
            "SELECT id, _score FROM pages \
              WHERE status IN ('accepted', 'draft') \
                AND multi_field_match(title, body, $1, 2.0, 1.0) \
              ORDER BY _score DESC LIMIT 10",
            &fusion_param(),
        )
        .unwrap();

    // Legacy dispatch fallback: column arithmetic cannot lower to the
    // operator tree, so the whole WHERE runs through the row evaluator.
    let legacy = eng
        .sql(
            "SELECT id, _score FROM pages \
              WHERE (id + 0) >= 0 \
                AND multi_field_match(title, body, $1, 2.0, 1.0) \
              ORDER BY _score DESC",
            &fusion_param(),
        )
        .unwrap();

    let bare = scored_ids(&bare);
    assert_eq!(
        bare,
        scored_ids(&mixed),
        "a pass-all ordinary predicate must not change match scores",
    );
    assert_eq!(
        bare,
        scored_ids(&legacy),
        "the operator-tree pipeline and the legacy dispatch must score identically",
    );
}

#[test]
fn mixed_predicate_multi_field_match_keeps_field_weight_monotonicity() {
    let eng = engine_with_small_corpus();

    let result = eng
        .sql(
            "SELECT id, _score FROM pages \
              WHERE status IN ('accepted', 'draft') \
                AND multi_field_match(title, body, $1, 2.0, 1.0) \
              ORDER BY _score DESC LIMIT 10",
            &fusion_param(),
        )
        .unwrap();

    let scored = scored_ids(&result);
    assert_eq!(
        scored.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![1, 2],
        "matching title and body must outrank matching body alone: {scored:?}",
    );
    assert!(
        scored[0].1 > scored[1].1,
        "the title match must add strictly positive evidence: {scored:?}",
    );
}
