//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 semantics encoded as engine tests.
//!
//! Every expectation in this file was verified against a live
//! `PostgreSQL` 18.4 instance (the `uqa-pg18` differential-testing
//! container driven by `tests/parity/pg18/run_diff.py`); the tests
//! themselves run without docker.

use uqa_core::{ArrayValue, DecimalValue, TemporalValue, Value};
use uqa_engine::Engine;

fn engine() -> Engine {
    Engine::new()
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let column = result.columns.first().expect("one column").clone();
    result.rows[0].get(&column).cloned().unwrap_or(Value::Null)
}

fn scalar_err(engine: &Engine, sql: &str) -> String {
    engine.sql(sql, &[]).unwrap_err().to_string()
}

fn text(engine: &Engine, sql: &str) -> String {
    match scalar(engine, sql) {
        Value::Str(s) => s,
        Value::Temporal(t) => t.to_sql_string(),
        Value::Decimal(d) => d.to_sql_string(),
        other => panic!("expected text-like value for {sql}, got {other:?}"),
    }
}

fn dec(text: &str) -> Value {
    Value::Decimal(DecimalValue::parse(text).unwrap())
}

fn array(elements: Vec<Value>) -> Value {
    Value::Array(ArrayValue::try_new(elements).unwrap())
}

fn bounded_array(elements: Vec<Value>, lower_bounds: Vec<i32>) -> Value {
    Value::Array(ArrayValue::with_lower_bounds(elements, lower_bounds).unwrap())
}

#[path = "pg18_semantics/arithmetic_and_casts.rs"]
mod arithmetic_and_casts;
#[path = "pg18_semantics/array_containment.rs"]
mod array_containment;
#[path = "pg18_semantics/array_transforms.rs"]
mod array_transforms;
#[path = "pg18_semantics/checksums.rs"]
mod checksums;
#[path = "pg18_semantics/comparisons_and_arrays.rs"]
mod comparisons_and_arrays;
#[path = "pg18_semantics/md5_overloads.rs"]
mod md5_overloads;
#[path = "pg18_semantics/numeric_exactness.rs"]
mod numeric_exactness;
#[path = "pg18_semantics/numeric_power_statistics.rs"]
mod numeric_power_statistics;
#[path = "pg18_semantics/pattern_escape.rs"]
mod pattern_escape;
#[path = "pg18_semantics/pg18_additions.rs"]
mod pg18_additions;
#[path = "pg18_semantics/reverse_overloads.rs"]
mod reverse_overloads;
#[path = "pg18_semantics/review_regressions.rs"]
mod review_regressions;
#[path = "pg18_semantics/string_binary_lengths.rs"]
mod string_binary_lengths;
#[path = "pg18_semantics/strings_and_bytea.rs"]
mod strings_and_bytea;
#[path = "pg18_semantics/temporal.rs"]
mod temporal;
#[path = "pg18_semantics/three_valued_logic.rs"]
mod three_valued_logic;
