//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::node_tree;
use uqa_core::DatumValue;

fn physical(id: &str) -> Value {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("../../range/datum/fixtures.json")).unwrap();
    let case = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap();
    let node_tree::Field::Node(node) = node_tree::parse(case["node"].as_str().unwrap()).unwrap()
    else {
        panic!("constant node");
    };
    let node_tree::Field::Datum { bytes, .. } = node.field("constvalue").unwrap() else {
        panic!("constant datum");
    };
    Value::Datum(DatumValue::new(
        case["oid"].as_u64().unwrap() as u32,
        0,
        bytes.clone(),
    ))
}

#[test]
fn physical_range_accessors_and_variadic_arrays_observe_admitted_postgresql_bytes() {
    for (id, multirange, upper) in [
        ("int4range_bounded", false, 3),
        ("int4multirange_bounded", true, 8),
    ] {
        let value = physical(id);
        for (operation, expected) in [
            (RangeFunctionOperation::Lower, Value::Int(1)),
            (RangeFunctionOperation::Upper, Value::Int(upper)),
            (RangeFunctionOperation::IsEmpty, Value::Bool(false)),
        ] {
            assert_eq!(
                eval_dispatched_range_function(
                    operation,
                    RangeSubtype::Integer,
                    multirange,
                    std::slice::from_ref(&value)
                )
                .unwrap(),
                expected
            );
        }
    }
    let array = physical("int4range_array_nullable");
    let error = eval_range_functions("int4multirange", &[array])
        .unwrap()
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22004"));
    assert_eq!(
        error.to_string(),
        "multirange values cannot contain null members"
    );
    let value = physical("int4range_bounded");
    assert_eq!(
        eval_range_functions("int4multirange", &[value])
            .unwrap()
            .unwrap(),
        Value::Str("{[1,3)}".into())
    );
}

#[test]
fn physical_range_observation_releases_quota_and_preserves_lazy_nulls() {
    let value = physical("int4multirange_bounded");
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let error = eval_dispatched_range_function_with_control(
        RangeFunctionOperation::Lower,
        RangeSubtype::Integer,
        true,
        &[value],
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(memory.used(), 0);
    let invalid = Value::Datum(DatumValue::new(3904, 0, vec![]));
    assert_eq!(
        eval_dispatched_range_function(
            RangeFunctionOperation::Overlap,
            RangeSubtype::Integer,
            false,
            &[invalid, Value::Null]
        )
        .unwrap(),
        Value::Null
    );
}
