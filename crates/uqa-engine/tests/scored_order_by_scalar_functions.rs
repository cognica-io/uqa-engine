//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Registered scalar functions must be usable inside ORDER BY of
//! scored-match queries, including over document columns that the
//! SELECT projection drops.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLParam;

fn engine_with_indexed_corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE pages (id INTEGER PRIMARY KEY, title TEXT, body TEXT, updated_at BIGINT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX pages_text ON pages USING gin (title, body)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO pages (id, title, body, updated_at) VALUES \
         (1, 'fusion scoring', 'fusion ranking in depth', 100), \
         (2, 'daily journal', 'one fusion mention only here', 900)",
        &[],
    )
    .unwrap();
    eng.register_scalar_function("order_boost", |args: &[Value]| {
        let base = match args.first() {
            Some(Value::Int(value)) => *value as f64,
            Some(Value::Float(value)) => *value,
            Some(Value::Decimal(value)) => value.to_f64().unwrap_or(0.0),
            other => {
                return Err(uqa_sql::SQLError::TypeMismatch(format!(
                    "order_boost expects a numeric argument, got {other:?}"
                )))
            }
        };
        Ok(Value::Float(base / 1_000.0))
    })
    .unwrap();
    eng
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

#[test]
fn scored_order_by_uses_registered_scalar_functions() {
    let eng = engine_with_indexed_corpus();
    let params = vec![SQLParam::scalar(Value::Str("fusion".into()))];

    let by_score = eng
        .sql(
            "SELECT id, _score FROM pages WHERE bayesian_match(body, $1) ORDER BY _score DESC",
            &params,
        )
        .unwrap();
    assert_eq!(ids(&by_score), vec![1, 2]);

    let boosted = eng
        .sql(
            "SELECT id, _score FROM pages WHERE bayesian_match(body, $1) \
              ORDER BY _score + order_boost(updated_at) DESC",
            &params,
        )
        .unwrap();
    assert_eq!(
        ids(&boosted),
        vec![2, 1],
        "the registered boost over the unprojected updated_at column must flip the order",
    );
    for row in &boosted.rows {
        assert!(
            row.keys().all(|key| !key.starts_with("__uqa_order_key_")),
            "materialised order keys must not leak into result rows: {row:?}",
        );
        assert_eq!(row.len(), 2, "only projected columns may remain: {row:?}");
    }
}

#[test]
fn plain_order_by_uses_registered_scalar_functions() {
    let eng = engine_with_indexed_corpus();

    let ordered = eng
        .sql(
            "SELECT id FROM pages ORDER BY order_boost(updated_at) DESC",
            &[],
        )
        .unwrap();
    assert_eq!(ids(&ordered), vec![2, 1]);
}
