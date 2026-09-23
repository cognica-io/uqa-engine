//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_operator_types_preserve_exact_unknown_and_cross_type_selection() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let cases = [
        (
            BinaryOp::Add,
            Some(ColumnType::Integer),
            None,
            [
                ColumnType::Integer,
                ColumnType::Integer,
                ColumnType::Integer,
            ],
        ),
        (
            BinaryOp::Add,
            Some(ColumnType::SmallInteger),
            Some(ColumnType::BigInteger),
            [
                ColumnType::SmallInteger,
                ColumnType::BigInteger,
                ColumnType::BigInteger,
            ],
        ),
        (
            BinaryOp::Subtract,
            Some(ColumnType::Date),
            Some(ColumnType::Date),
            [ColumnType::Date, ColumnType::Date, ColumnType::Integer],
        ),
        (
            BinaryOp::Equal,
            Some(ColumnType::Text),
            Some(ColumnType::Text),
            [ColumnType::Text, ColumnType::Text, ColumnType::Boolean],
        ),
    ];
    for (op, left, right, expected) in cases {
        let output =
            binary_operator_types_with_control(op, left.as_ref(), right.as_ref(), &control)
                .unwrap();
        assert_eq!(*output, expected);
        assert_eq!(
            binary_operator_types(op, left.as_ref(), right.as_ref()).unwrap(),
            expected
        );
        assert_eq!(output.reserved_bytes(), 0);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn polymorphic_operator_outputs_keep_only_their_nested_type_leases() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let input = ColumnType::Array(Box::new(ColumnType::Varchar(Some(12))));
    let output =
        binary_operator_types_with_control(BinaryOp::Equal, Some(&input), Some(&input), &control)
            .unwrap();
    assert_eq!(
        *output,
        [
            ColumnType::Array(Box::new(ColumnType::Varchar(None))),
            ColumnType::Array(Box::new(ColumnType::Varchar(None))),
            ColumnType::Boolean
        ]
    );
    assert_eq!(output.reserved_bytes(), 2 * size_of::<ColumnType>());
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn operator_workspace_failure_is_not_rewritten_as_undefined_operator() {
    for limit in [0, 32, 128, 512, 4096, 65536] {
        let budget = MemoryBudget::new(limit);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        match binary_operator_types_with_control(
            BinaryOp::Add,
            Some(&ColumnType::Integer),
            Some(&ColumnType::Integer),
            &control,
        ) {
            Ok(output) => {
                assert_eq!(output[2], ColumnType::Integer);
                drop(output);
            }
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let error = binary_operator_types_with_control(
        BinaryOp::Add,
        Some(&ColumnType::Boolean),
        Some(&ColumnType::Date),
        &ProductionControl::new(&budget, &cancellation, &cancellation),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn either_cancellation_stops_operator_type_production_without_retention() {
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = binary_operator_types_with_control(
            BinaryOp::Add,
            Some(&ColumnType::Integer),
            Some(&ColumnType::Integer),
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}
