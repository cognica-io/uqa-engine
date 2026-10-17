//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionVec},
    CancellationToken, DatumValue, DecimalValue,
};

fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!("tests/postgresql.json")).unwrap()
}

#[test]
fn range_subtype_distances_match_postgresql_float_bits_and_diagnostics() {
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for case in reference()["cases"].as_array().unwrap() {
        let name = case["function"].as_str().unwrap();
        let target = case["type"].as_str().unwrap();
        let mut arguments = ProductionVec::new(control);
        for argument in case["arguments"].as_array().unwrap() {
            let value = argument
                .as_str()
                .map_or(Value::Null, |text| Value::Str(text.into()));
            arguments
                .push_produced(
                    crate::expr::cast_value_from_with_control(&value, target, None, &control)
                        .unwrap(),
                )
                .unwrap();
        }
        let arguments = arguments.finish().unwrap();
        let expected = &case["expected"];
        let result = evaluate(name, &arguments, &control).unwrap();
        if let Some(expected) = expected.get("error") {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), expected["sqlstate"].as_str());
            let SQLError::Routine { message, .. } = error else {
                panic!("expected scalar range diagnostic");
            };
            assert_eq!(message, expected["message"].as_str().unwrap());
        } else {
            let result = result.unwrap();
            if let Some(bits) = expected["hex"].as_str() {
                let Value::Float(value) = *result else {
                    panic!("expected float8")
                };
                assert_eq!(
                    value.to_bits(),
                    u64::from_str_radix(bits, 16).unwrap(),
                    "{case}"
                );
            } else {
                assert_eq!(*result, Value::Null);
            }
            assert_eq!(result.reserved_bytes(), 0);
        }
        drop(arguments);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn range_subtype_distances_read_only_owned_physical_endpoint_bytes() {
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, oid, left, right, expected) in [
        (
            "int4range_subdiff",
            23,
            i64::from(i32::MAX),
            i64::from(i32::MIN),
            4_294_967_295.0,
        ),
        ("int8range_subdiff", 20, i64::MAX, i64::MAX - 1, 0.0),
        (
            "daterange_subdiff",
            1082,
            i64::from(i32::MAX),
            0,
            2_147_483_647.0,
        ),
        ("tsrange_subdiff", 1114, 1, 0, 0.000_001),
        ("tstzrange_subdiff", 1184, i64::MAX, i64::MAX, 0.0),
    ] {
        let datum =
            |value: i64| Value::Datum(DatumValue::new(oid, 0, value.to_le_bytes().to_vec()));
        let result = evaluate(name, &[datum(left), datum(right)], &control)
            .unwrap()
            .unwrap();
        assert_eq!(*result, Value::Float(expected));
        let truncated = Value::Datum(DatumValue::new(oid, 0, vec![0]));
        let error = evaluate(name, &[truncated, datum(right)], &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX001"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn numeric_range_distance_keeps_intermediates_within_the_original_allowance() {
    let number = DecimalValue::parse(&format!("1{}", "0".repeat(8192))).unwrap();
    let arguments = [Value::Decimal(number.clone()), Value::Decimal(number)];
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for limit in [512, 1024 * 1024] {
        let budget = MemoryBudget::new(limit);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let result = evaluate("numrange_subdiff", &arguments, &control).unwrap();
        if limit == 512 {
            assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        } else {
            assert_eq!(*result.unwrap(), Value::Float(0.0));
        }
        assert_eq!(budget.used(), 0);
    }
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let budget = MemoryBudget::new(0);
        let control = ProductionControl::new(&budget, &original, &invoking);
        for (name, arguments) in [
            ("numrange_subdiff", arguments.as_slice()),
            ("int4range_subdiff", &[Value::Null, Value::Null]),
        ] {
            assert_eq!(
                evaluate(name, arguments, &control)
                    .unwrap()
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
        }
        assert_eq!(budget.used(), 0);
    }
}
