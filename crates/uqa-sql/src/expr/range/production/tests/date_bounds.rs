//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn date_range_successors_preserve_postgresql_finite_bounds_and_empty_order() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("date_bounds.json")).unwrap();
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for case in reference["cases"].as_array().unwrap() {
        let text = case["input"].as_str().unwrap();
        let result = if case["kind"] == "datemultirange" {
            canonical_multirange_text_with_control(text, RangeSubtype::Date, &control)
        } else {
            canonical_range_text_with_control(text, RangeSubtype::Date, &control)
        };
        if let Some(expected) = case["error"].as_object() {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), expected["sqlstate"].as_str(), "{case}");
            assert_eq!(error.to_string(), expected["message"].as_str().unwrap());
            assert_eq!(error.detail(), expected["detail"].as_str());
            assert_eq!(error.hint(), expected["hint"].as_str());
        } else {
            let result = result.unwrap_or_else(|error| panic!("{case}: {error}"));
            assert_eq!(*result, case["expected"].as_str().unwrap(), "{case}");
            assert_eq!(memory.used(), result.reserved_bytes());
        }
        assert_eq!(memory.used(), 0);
    }
}
