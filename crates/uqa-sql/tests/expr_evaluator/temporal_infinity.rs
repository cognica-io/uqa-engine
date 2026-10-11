//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` 18 observations for non-finite temporal values.

use super::{eval, projection_expr, EvalContext, Value};

#[derive(serde::Deserialize)]
struct Case {
    group: String,
    sql: String,
    expected: Option<String>,
    error: Option<Diagnostic>,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct Diagnostic {
    sqlstate: String,
    message: String,
    detail: Option<String>,
    hint: Option<String>,
}

fn check(group: &str) {
    let cases: Vec<Case> = serde_json::from_str(include_str!("temporal_infinity.json")).unwrap();
    let mut failures = Vec::new();
    for case in cases.iter().filter(|case| case.group == group) {
        let observed = eval(&projection_expr(&case.sql), &EvalContext::new(None, &[]));
        let matches = match (&observed, &case.error) {
            (Ok(value), None) => {
                *value
                    == case
                        .expected
                        .as_ref()
                        .map_or(Value::Null, |s| Value::Str(s.clone()))
            }
            (Err(error), Some(expected)) => {
                error.sqlstate() == Some(expected.sqlstate.as_str())
                    && error.to_string() == expected.message
                    && error.detail() == expected.detail.as_deref()
                    && error.hint() == expected.hint.as_deref()
            }
            _ => false,
        };
        if !matches {
            failures.push(format!(
                "{}\nexpected: {:?} {:?}\nobserved: {observed:?}",
                case.sql, case.expected, case.error
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

macro_rules! groups {
    ($($group:ident),+ $(,)?) => { $(
        #[test]
        fn $group() { check(stringify!($group)); }
    )+ };
}

groups!(input, cast, compare, arithmetic, extract, trunc, function, zone, finite);
