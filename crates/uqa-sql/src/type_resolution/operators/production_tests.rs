//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::type_resolution::equality_operand_type_with_control;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn unary_and_inline_equality_types_need_no_heap_allowance() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    for ty in [ColumnType::Integer, ColumnType::Real, ColumnType::Interval] {
        assert_eq!(
            *unary_minus_result_type_with_control(&ty, &control).unwrap(),
            ty
        );
    }
    assert_eq!(
        *equality_operand_type_with_control(
            &ColumnType::Text,
            &ColumnType::Varchar(Some(12)),
            &control
        )
        .unwrap(),
        ColumnType::Text
    );
    assert_eq!(
        *binary_result_type_with_control(
            BinaryOp::Equal,
            Some(&ColumnType::Vector(3)),
            Some(&ColumnType::Vector(3)),
            &control
        )
        .unwrap()
        .unwrap(),
        ColumnType::Boolean
    );
    assert_eq!(budget.used(), 0);
    assert_eq!(budget.peak(), 0);
}

#[test]
fn equality_array_type_retains_declared_modifiers_and_owns_one_result_copy() {
    let left = ColumnType::Array(Box::new(ColumnType::Varchar(Some(12))));
    let right = ColumnType::Array(Box::new(ColumnType::Varchar(Some(48))));
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let ty = equality_operand_type_with_control(&left, &right, &control).unwrap();
    assert_eq!(*ty, left);
    assert_eq!(ty.reserved_bytes(), size_of::<ColumnType>());
    assert_eq!(budget.used(), ty.reserved_bytes());
    drop(ty);
    assert_eq!(budget.used(), 0);
}

#[test]
fn type_result_errors_preserve_resource_identity_and_both_cancellation_sources() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let array = ColumnType::Array(Box::new(ColumnType::Integer));
    assert_eq!(
        equality_operand_type_with_control(&array, &array, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(
        binary_result_type_with_control(
            BinaryOp::Add,
            Some(&ColumnType::Integer),
            Some(&ColumnType::Integer),
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            unary_minus_result_type_with_control(&ColumnType::Integer, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            equality_operand_type_with_control(
                &ColumnType::Integer,
                &ColumnType::Integer,
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            binary_result_type_with_control(BinaryOp::Equal, Some(&array), Some(&array), &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
    }
    assert_eq!(budget.used(), 0);
}
