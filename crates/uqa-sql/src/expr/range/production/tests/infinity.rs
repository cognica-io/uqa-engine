//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` range bounds, emptiness and error-order observations.

use super::*;

#[derive(Debug, serde::Deserialize)]
struct Case {
    kind: String,
    input: String,
    expected: Option<String>,
    error: Option<Diagnostic>,
}

#[derive(Debug, serde::Deserialize)]
struct Diagnostic {
    sqlstate: String,
    message: String,
    detail: Option<String>,
    hint: Option<String>,
}

#[test]
fn range_bounds_match_postgresql_infinity_and_canonicalization_order() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("infinity.json")).unwrap();
    assert_eq!(cases.len(), 138);
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut failures = Vec::new();
    for case in cases {
        let subtype = match case.kind.as_str() {
            "int4range" => RangeSubtype::Integer,
            "int8range" => RangeSubtype::BigInteger,
            "numrange" => RangeSubtype::Numeric,
            "daterange" | "datemultirange" => RangeSubtype::Date,
            "tsrange" => RangeSubtype::Timestamp,
            "tstzrange" => RangeSubtype::TimestampTz,
            _ => unreachable!(),
        };
        let observed = if case.kind == "datemultirange" {
            canonical_multirange_text_with_control(&case.input, subtype, &control)
        } else {
            canonical_range_text_with_control(&case.input, subtype, &control)
        };
        let matches = match (&observed, &case.error) {
            (Ok(value), None) => Some(value.as_str()) == case.expected.as_deref(),
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
        drop(observed);
        assert_eq!(memory.used(), 0);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
