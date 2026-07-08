//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-field fusion monotonicity: matching an additional field must
//! never rank a document below one that matched fewer fields. With the
//! old 0.5 neutral padding, calibrated matched posteriors on small
//! corpora sat below the pad and inverted the field weights.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLParam;

fn fusion_param() -> Vec<SQLParam> {
    vec![SQLParam::scalar(Value::Str("fusion".into()))]
}

fn ids(result: &uqa_sql::SQLResult) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(value)) => *value,
            other => panic!("expected Int id, got {other:?}"),
        })
        .collect()
}

fn engine_with_small_corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE pages (id INTEGER PRIMARY KEY, title TEXT, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX pages_text ON pages USING gin (title, body)",
        &[],
    )
    .unwrap();
    // Filler rows keep the corpus statistics realistic for a personal
    // library: most documents match nothing.
    for index in 0..6 {
        eng.sql(
            &format!(
                "INSERT INTO pages (id, title, body) VALUES \
                 ({}, 'grocery list {index}', 'apples rice and beans for week {index}')",
                100 + index
            ),
            &[],
        )
        .unwrap();
    }
    eng.sql(
        "INSERT INTO pages (id, title, body) VALUES \
         (1, 'fusion scoring', 'fusion ranking covered in depth with examples'), \
         (2, 'daily journal', 'one fusion mention only in passing today')",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn title_and_body_match_outranks_body_only_match() {
    let eng = engine_with_small_corpus();

    let result = eng
        .sql(
            "SELECT id, _score FROM pages \
              WHERE multi_field_match(title, body, $1, 2.0, 1.0) \
              ORDER BY _score DESC",
            &fusion_param(),
        )
        .unwrap();

    assert_eq!(
        ids(&result),
        vec![1, 2],
        "matching title and body must outrank matching body alone: {:?}",
        result.rows,
    );
}

#[test]
fn matching_an_extra_field_never_lowers_the_score() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX docs_text ON docs USING gin (title, body)",
        &[],
    )
    .unwrap();
    // Identical bodies; only doc 1 also matches on title.
    eng.sql(
        "INSERT INTO docs (id, title, body) VALUES \
         (1, 'fusion overview', 'shared fusion body text'), \
         (2, 'unrelated title', 'shared fusion body text')",
        &[],
    )
    .unwrap();

    let result = eng
        .sql(
            "SELECT id, _score FROM docs \
              WHERE multi_field_match(title, body, $1, 2.0, 1.0) \
              ORDER BY _score DESC",
            &fusion_param(),
        )
        .unwrap();

    assert_eq!(ids(&result), vec![1, 2], "{:?}", result.rows);
    let scores: Vec<f64> = result
        .rows
        .iter()
        .map(|row| match row.get("_score") {
            Some(Value::Float(value)) => *value,
            other => panic!("expected Float score, got {other:?}"),
        })
        .collect();
    assert!(
        scores[0] > scores[1],
        "the title match must add strictly positive evidence: {scores:?}",
    );
}
