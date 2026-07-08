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

#[test]
fn decimal_weighted_blend_preserves_relevance_order() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE pages (id INTEGER PRIMARY KEY, status TEXT, body TEXT, updated_at BIGINT)",
        &[],
    )
    .unwrap();
    eng.sql("CREATE INDEX pages_text ON pages USING gin (body)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO pages (id, status, body, updated_at) VALUES \
         (1, 'accepted', 'fusion fusion fusion ranking in depth', 100), \
         (2, 'accepted', 'one fusion mention', 900)",
        &[],
    )
    .unwrap();
    eng.register_scalar_function("micro_boost", |args: &[Value]| {
        let base = match args.first() {
            Some(Value::Int(value)) => *value as f64,
            Some(Value::Float(value)) => *value,
            Some(Value::Decimal(value)) => value.to_f64().unwrap_or(0.0),
            other => {
                return Err(uqa_sql::SQLError::TypeMismatch(format!(
                    "micro_boost expects a numeric argument, got {other:?}"
                )))
            }
        };
        Ok(Value::Float(base / 1_000_000.0))
    })
    .unwrap();
    let params = vec![SQLParam::scalar(Value::Str("fusion".into()))];

    let plain = eng
        .sql(
            "SELECT id, _score FROM pages \
              WHERE status IN ('accepted', 'draft') AND bayesian_match(body, $1) \
              ORDER BY _score DESC LIMIT 10",
            &params,
        )
        .unwrap();
    assert_eq!(ids(&plain), vec![1, 2]);

    let blended = eng
        .sql(
            "SELECT id, _score FROM pages \
              WHERE status IN ('accepted', 'draft') AND bayesian_match(body, $1) \
              ORDER BY _score + 0.05 * micro_boost(updated_at) DESC, updated_at DESC \
              LIMIT 10",
            &params,
        )
        .unwrap();
    assert_eq!(
        ids(&blended),
        vec![1, 2],
        "a decimal-literal weight must not degrade the primary sort key; \
         with a tiny boost the relevance order has to survive the tiebreak",
    );
}

#[test]
fn plain_order_by_decimal_arithmetic_key() {
    let eng = engine_with_indexed_corpus();

    let ordered = eng
        .sql("SELECT id FROM pages ORDER BY updated_at * 0.5 DESC", &[])
        .unwrap();
    assert_eq!(
        ids(&ordered),
        vec![2, 1],
        "decimal-typed sort keys must order rows instead of comparing as equal",
    );
}

#[test]
fn top_k_order_by_decimal_arithmetic_key() {
    let eng = engine_with_indexed_corpus();

    // LIMIT below the row count routes through the top-k shortcut,
    // which has its own sort comparator separate from the Volcano sort.
    let ordered = eng
        .sql(
            "SELECT id FROM pages ORDER BY updated_at * 0.5 DESC LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(
        ids(&ordered),
        vec![2],
        "the top-k shortcut must compare decimal sort keys instead of treating them as equal",
    );
}
