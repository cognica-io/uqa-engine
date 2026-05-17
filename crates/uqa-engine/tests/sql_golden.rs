//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Golden-file SQL parity harness.
//!
//! Loads `tests/parity/sql_golden_fixture.json` and replays every
//! case against a fresh in-memory `Engine`, asserting the result rows
//! match the expected values. The fixture is intentionally
//! engine-agnostic so the same JSON can be regenerated from the
//! canonical UQA behavior implementation when we want to lock down parity
//! at a deeper level.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value as JSONValue;
use uqa_core::Value;
use uqa_engine::Engine;

#[derive(Deserialize)]
struct Fixture {
    #[allow(dead_code)]
    version: u32,
    schema_sql: Vec<String>,
    data_sql: Vec<String>,
    cases: Vec<GoldenCase>,
}

#[derive(Deserialize)]
struct GoldenCase {
    name: String,
    sql: String,
    expected: Vec<JSONValue>,
}

fn fixture_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests")
        .join("parity")
        .join("sql_golden_fixture.json")
}

fn load_fixture() -> Fixture {
    let bytes = std::fs::read(fixture_path()).expect("fixture present");
    serde_json::from_slice(&bytes).expect("fixture parses")
}

fn json_to_value(v: &JSONValue) -> Value {
    match v {
        JSONValue::Null | JSONValue::Object(_) => Value::Null,
        JSONValue::Bool(b) => Value::Bool(*b),
        JSONValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        JSONValue::String(s) => Value::Str(s.clone()),
        JSONValue::Array(items) => Value::List(items.iter().map(json_to_value).collect()),
    }
}

#[test]
fn sql_golden_fixture_passes() {
    let fx = load_fixture();
    let engine = Engine::new();
    for stmt in &fx.schema_sql {
        engine.sql(stmt, &[]).expect("schema sql");
    }
    for stmt in &fx.data_sql {
        engine.sql(stmt, &[]).expect("data sql");
    }
    for case in &fx.cases {
        let result = engine
            .sql(&case.sql, &[])
            .unwrap_or_else(|e| panic!("[{}] sql error: {e}", case.name));
        assert_eq!(
            result.rows.len(),
            case.expected.len(),
            "[{}] row count differs: got {}, expected {}",
            case.name,
            result.rows.len(),
            case.expected.len()
        );
        for (i, (got, exp)) in result.rows.iter().zip(case.expected.iter()).enumerate() {
            let exp_obj = exp.as_object().unwrap_or_else(|| {
                panic!("[{}] expected row {i} is not an object: {exp}", case.name)
            });
            for (column, expected_json) in exp_obj {
                let expected = json_to_value(expected_json);
                let actual = got.get(column).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    actual, expected,
                    "[{} idx {i}] column {column:?}: got {:?}, expected {:?}",
                    case.name, actual, expected
                );
            }
        }
    }
}
