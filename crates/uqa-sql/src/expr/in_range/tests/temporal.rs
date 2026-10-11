//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` support-function observations include finite controls.

use super::{in_range, TemporalValue, Value};

#[derive(Debug, serde::Deserialize)]
struct Case {
    kind: String,
    value: String,
    base: String,
    value_days: Option<i32>,
    base_days: Option<i32>,
    offset: String,
    subtract: bool,
    less: bool,
    expected: Option<bool>,
    error: Option<Diagnostic>,
}

#[derive(Debug, serde::Deserialize)]
struct Diagnostic {
    sqlstate: String,
    message: String,
    detail: Option<String>,
    hint: Option<String>,
}

fn temporal(kind: &str, text: &str, date_days: Option<i32>) -> Value {
    // The reference records PostgreSQL's Unix-day difference for dates outside
    // its timestamp range, independently of the ordinary text reader's limits.
    if let Some(days) = date_days {
        return Value::Temporal(TemporalValue::Date { days });
    }
    let value = match kind {
        "date" => TemporalValue::parse_date(text),
        "timestamp" => TemporalValue::parse_timestamp(text),
        "timestamptz" => TemporalValue::parse_timestamp_tz(text),
        "interval" => TemporalValue::parse_interval(text),
        "time" => TemporalValue::parse_time(text),
        "timetz" => TemporalValue::parse_time_tz(text),
        _ => unreachable!(),
    };
    Value::Temporal(value.unwrap_or_else(|| panic!("{text}::{kind}")))
}

#[test]
fn temporal_bounds_match_postgresql_infinite_and_finite_observations() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("temporal.json")).unwrap();
    assert_eq!(cases.len(), 860);
    let mut failures = Vec::new();
    for case in cases {
        let value = temporal(&case.kind, &case.value, case.value_days);
        let base = temporal(&case.kind, &case.base, case.base_days);
        let offset = temporal("interval", &case.offset, None);
        let observed = in_range(&value, &base, &offset, case.subtract, case.less);
        let matches = match (&observed, &case.error) {
            (Ok(value), None) => Some(*value) == case.expected,
            (Err(error), Some(expected)) => {
                error.sqlstate() == Some(expected.sqlstate.as_str())
                    && error.to_string() == expected.message
                    && error.detail() == expected.detail.as_deref()
                    && error.hint() == expected.hint.as_deref()
            }
            _ => false,
        };
        if !matches {
            failures.push(format!("{case:?}\nobserved: {observed:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
