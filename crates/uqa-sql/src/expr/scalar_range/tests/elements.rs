//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently captured element containment, including temporal and numeric infinity.

use super::*;
use crate::ColumnType;

#[test]
fn ordinary_and_controlled_elements_match_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!("elements.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 376);
    let memory = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut count = 0;
    for case in cases.iter().filter(|case| case["owner"] == true) {
        let ty = ColumnType::from_sql_name(case["range_type"].as_str().unwrap()).unwrap();
        let (subtype, multirange) = match ty {
            ColumnType::Range(subtype) => (subtype, false),
            ColumnType::Multirange(subtype) => (subtype, true),
            _ => unreachable!(),
        };
        let range = case["container"]
            .as_str()
            .map_or(Value::Null, |text| Value::Str(text.into()));
        let element = case["element"]
            .as_str()
            .map_or(Value::Null, |text| Value::Str(text.into()));
        let element =
            crate::expr::cast_value(&element, case["scalar_type"].as_str().unwrap()).unwrap();
        for (column, operation, args) in [
            (
                0,
                RangeFunctionOperation::ContainsElement,
                [range.clone(), element.clone()],
            ),
            (
                1,
                RangeFunctionOperation::ElementContainedBy,
                [element.clone(), range.clone()],
            ),
        ] {
            let expected = case["results"][0]["rows"][0][column]
                .as_str()
                .map_or(Value::Null, |value| Value::Bool(value == "t"));
            assert_eq!(
                eval_dispatched_range_function(operation, subtype, multirange, &args).unwrap(),
                expected,
                "{case}"
            );
            let output = eval_dispatched_range_function_with_control(
                operation, subtype, multirange, &args, &control,
            )
            .unwrap();
            assert_eq!(*output, expected, "{case}");
            drop(output);
            assert_eq!(memory.used(), 0);
        }
        count += 1;
    }
    assert_eq!(count, 300);
}

#[test]
fn element_containment_releases_scratch_on_quota_failure_and_checks_both_cancellations() {
    let memory = MemoryBudget::new(8192);
    let args = [
        Value::Str("{[1.00000000000000000001,2.00000000000000000001)}".into()),
        Value::Decimal(DecimalValue::parse("1.00000000000000000002").unwrap()),
    ];
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let held = memory.reserve(memory.limit()).unwrap();
        let error = eval_dispatched_range_function_with_control(
            RangeFunctionOperation::ContainsElement,
            RangeSubtype::Numeric,
            true,
            &args,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(memory.used(), held.bytes());
        drop(held);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = eval_dispatched_range_function_with_control(
            RangeFunctionOperation::ContainsElement,
            RangeSubtype::Numeric,
            true,
            &args,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}
