//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Empty range containment follows the same point-set inclusion as multiranges.

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn ordinary_and_controlled_empty_containment_match_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!("empty.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 66);
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for case in cases.iter().filter(|case| case.get("op").is_some()) {
        let operation = if case["op"] == "@>" {
            RangeFunctionOperation::Contains
        } else {
            RangeFunctionOperation::ContainedBy
        };
        let subtype = if case["kind"] == "date" {
            RangeSubtype::Date
        } else {
            RangeSubtype::Integer
        };
        let left = case["left"].as_str().unwrap();
        let args = [
            Value::Str(left.into()),
            Value::Str(case["right"].as_str().unwrap().into()),
        ];
        let multirange = left.starts_with('{');
        let expected = Value::Bool(case["results"][0]["rows"][0][0] == "t");
        assert_eq!(
            eval_dispatched_range_function(operation, subtype, multirange, &args).unwrap(),
            expected,
            "{case}"
        );
        let value = eval_dispatched_range_function_with_control(
            operation, subtype, multirange, &args, &control,
        )
        .unwrap();
        assert_eq!(*value, expected, "{case}");
        drop(value);
        assert_eq!(memory.used(), 0);
    }
    token.cancel();
    assert!(eval_dispatched_range_function_with_control(
        RangeFunctionOperation::Contains,
        RangeSubtype::Integer,
        true,
        &[Value::Str("{}".into()), Value::Str("empty".into())],
        &control
    )
    .is_err());
    assert_eq!(memory.used(), 0);
}
