//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent array input diagnostics and bounded parser ownership.

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn array_literal_diagnostics_and_values_match_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/array_literal_diagnostics_oracle.expected.json"
    )))
    .unwrap();
    for case in reference["cases"].as_array().unwrap().iter().skip(1) {
        if !case["id"].as_str().unwrap().starts_with("array_") {
            continue;
        }
        let sql = case["sql"].as_str().unwrap();
        let crate::Statement::Select(query) = crate::compile(sql).unwrap().remove(0) else {
            panic!("array input reference is SELECT");
        };
        let result = crate::expr::eval(
            &query.projections[0].expr,
            &crate::expr::EvalContext::new(None, &[]),
        );
        if case["error"].is_null() {
            let value = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
            let text = crate::expr::value_to_string(&value).unwrap();
            assert_eq!(text, case["results"][0]["rows"][0][0], "{sql}");
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                case["error"]["sqlstate"].as_str(),
                "{sql}"
            );
            assert_eq!(error.to_string(), case["error"]["message"], "{sql}");
            assert_eq!(error.detail(), case["error"]["detail"].as_str(), "{sql}");
            assert_eq!(error.hint(), case["error"]["hint"].as_str(), "{sql}");
        }
    }
}

#[test]
fn array_literal_errors_release_admitted_buffers_and_preserve_cancellation() {
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for text in ["{1,", "{{1},{2,3}}", "[1:2]={1}", "{\"unterminated"] {
        let error = parse_pg_array_literal_with_control(text, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P02"));
        assert!(error.detail().is_some());
        assert_eq!(budget.used(), 0, "{text}");
    }
    let input = format!("{{{}", "x".repeat(8192));
    let error = parse_pg_array_literal_with_control(&input, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    invoking.cancel();
    let error = parse_pg_array_literal_with_control("{}", &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(budget.used(), 0);
}
