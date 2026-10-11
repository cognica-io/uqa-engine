//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Timestamp conversions captured from `PostgreSQL` 18.4 in Docker.

use super::{eval, projection_expr, EvalContext, Value};
use uqa_sql::{error::Result, expr::EngineHook};

#[derive(serde::Deserialize)]
struct Case {
    group: String,
    sql: String,
    session_zone: String,
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

struct Session<'a>(&'a str);
impl EngineHook for Session<'_> {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn runtime_parameter(&self, name: &str) -> Result<Option<String>> {
        Ok((name == "TimeZone").then(|| self.0.into()))
    }
}

fn check(group: &str) {
    let cases: Vec<Case> = serde_json::from_str(include_str!("timezone.json")).unwrap();
    let mut failures = Vec::new();
    for case in cases.iter().filter(|case| case.group == group) {
        let session = Session(&case.session_zone);
        let context = EvalContext::new(None, &[]).with_engine(&session);
        let observed = eval(&projection_expr(&case.sql), &context);
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

groups!(
    named,
    transition,
    abbreviation,
    interval,
    nonfinite,
    diagnostic,
    null,
    local
);
