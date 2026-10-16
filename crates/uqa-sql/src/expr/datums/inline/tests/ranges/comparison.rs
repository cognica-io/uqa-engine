//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::{
    compare_typed_values_with_enum_catalog, equal_typed_values_with_enum_catalog,
    eval_comparison_truth_with_enum_catalog, BinaryOp,
};
use std::cmp::Ordering;
use uqa_core::{ArrayValue, RecordFieldType, RowValue};

fn reference() -> Reference {
    let reference =
        Reference::from_data(serde_json::from_str(include_str!("comparison.json")).unwrap());
    reference.observing.set(true);
    reference
}

fn values(reference: &Reference, name: &str, index: usize) -> Vec<Value> {
    let node = &case(&reference.data, &format!("{name}_comparison_catalog"))["results"][0]["rows"]
        [index][0];
    let value = crate::expr::datums::read_with_catalog(&datum(node), None);
    assert!(
        value.is_err(),
        "declared values require their pinned catalog"
    );
    let value = crate::expr::datums::read_with_value_catalog_and_control(
        &datum(node),
        Some(reference),
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
    .into_uncontrolled()
    .unwrap();
    let Value::Array(array) = value else {
        panic!("physical array")
    };
    array.elements().to_vec()
}

fn containers(value: &Value, oid: u32) -> [Value; 3] {
    [
        value.clone(),
        Value::Row(
            RowValue::typed(
                vec![value.clone()],
                vec![RecordFieldType {
                    oid,
                    type_modifier: -1,
                }],
            )
            .unwrap(),
        ),
        Value::Array(
            ArrayValue::try_new(vec![value.clone()])
                .unwrap()
                .with_element_type_oid(Some(oid)),
        ),
    ]
}

#[test]
fn declared_range_comparisons_follow_selected_postgresql_operations_in_all_consumers() {
    let reference = reference();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut observations = 0;
    for name in FAMILIES.into_iter().chain(["reverse_window"]) {
        for (index, column) in [(0, "r"), (1, "m")] {
            let oid = integer(&reference.metadata(name)[if index == 0 { 1 } else { 6 }]);
            let inputs = values(&reference, name, index);
            let expected = &case(&reference.data, &format!("{name}_comparison_{column}"))
                ["results"][0]["rows"];
            for row in expected.as_array().unwrap() {
                let left = row[0].as_str().unwrap().parse::<usize>().unwrap() - 1;
                let right = row[1].as_str().unwrap().parse::<usize>().unwrap() - 1;
                let order = row[2].as_str().unwrap().parse::<i8>().unwrap().cmp(&0);
                for (left, right) in containers(&inputs[left], oid)
                    .iter()
                    .zip(containers(&inputs[right], oid))
                {
                    assert_eq!(
                        compare_typed_values_with_enum_catalog(
                            left,
                            &right,
                            &control,
                            Some(&reference),
                            None
                        )
                        .unwrap(),
                        order,
                        "{name} {column}"
                    );
                    assert_eq!(
                        equal_typed_values_with_enum_catalog(
                            left,
                            &right,
                            &control,
                            Some(&reference)
                        )
                        .unwrap(),
                        order == Ordering::Equal,
                        "{name} {column}"
                    );
                    for (operator, expected) in [
                        (BinaryOp::Equal, order.is_eq()),
                        (BinaryOp::NotEqual, !order.is_eq()),
                        (BinaryOp::Less, order.is_lt()),
                        (BinaryOp::LessEqual, order.is_le()),
                        (BinaryOp::Greater, order.is_gt()),
                        (BinaryOp::GreaterEqual, order.is_ge()),
                    ] {
                        assert_eq!(
                            eval_comparison_truth_with_enum_catalog(
                                operator,
                                left,
                                &right,
                                &control,
                                Some(&reference),
                                None
                            )
                            .unwrap(),
                            Some(expected),
                            "{name} {column} {operator:?}"
                        );
                    }
                    assert_eq!(memory.used(), 0);
                    observations += 1;
                }
            }
        }
    }
    assert_eq!(observations, 1764);
    assert!(reference.comparisons.get() > 0);
}

#[test]
fn declared_range_comparison_preserves_lazy_errors_and_both_resource_owners() {
    let reference = reference();
    let inputs = values(&reference, "record_window", 0);
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    reference.fail_comparison.set(true);
    for (left, right) in [(&Value::Null, &inputs[2]), (&inputs[2], &Value::Null)] {
        for operator in [BinaryOp::Equal, BinaryOp::Less] {
            assert_eq!(
                eval_comparison_truth_with_enum_catalog(
                    operator,
                    left,
                    right,
                    &control,
                    Some(&reference),
                    None
                )
                .unwrap(),
                None
            );
        }
    }
    assert_eq!(
        compare_typed_values_with_enum_catalog(
            &inputs[0],
            &inputs[2],
            &control,
            Some(&reference),
            None
        )
        .unwrap(),
        Ordering::Less
    );
    assert_eq!(
        reference.comparisons.get(),
        0,
        "empty comparison must not invoke a bound operator"
    );
    let error = compare_typed_values_with_enum_catalog(
        &inputs[2],
        &inputs[3],
        &control,
        Some(&reference),
        None,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(error.to_string(), "selected comparison failed");
    assert_eq!(memory.used(), 0);
    reference.fail_comparison.set(false);
    for allowance in [0, 16, 64, 128, 512, 4096] {
        let memory = MemoryBudget::new(allowance);
        let result = compare_typed_values_with_enum_catalog(
            &inputs[3],
            &inputs[4],
            &ProductionControl::new(&memory, &original, &invoking),
            Some(&reference),
            None,
        );
        if let Err(error) = result {
            assert_eq!(error.sqlstate(), Some("53200"));
        }
        assert_eq!(memory.used(), 0);
    }
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = compare_typed_values_with_enum_catalog(
            &inputs[3],
            &inputs[4],
            &ProductionControl::new(&memory, &original, &invoking),
            Some(&reference),
            None,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}
