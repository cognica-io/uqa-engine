//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_numeric_selection_keeps_operand_widths_and_catalog_identities() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let numeric = ColumnType::Numeric {
        precision: None,
        scale: None,
    };
    let cases = [
        (
            NumericOperator::Plus,
            vec![Some(ColumnType::Integer)],
            ColumnType::Integer,
            1918,
            1912,
        ),
        (
            NumericOperator::Absolute,
            vec![Some(ColumnType::SmallInteger)],
            ColumnType::SmallInteger,
            682,
            1253,
        ),
        (
            NumericOperator::Modulo,
            vec![Some(ColumnType::BigInteger), Some(ColumnType::BigInteger)],
            ColumnType::BigInteger,
            439,
            945,
        ),
        (
            NumericOperator::Power,
            vec![Some(numeric.clone()), Some(numeric.clone())],
            numeric.clone(),
            1038,
            1739,
        ),
        (
            NumericOperator::SquareRoot,
            vec![Some(numeric)],
            ColumnType::DoublePrecision,
            596,
            230,
        ),
        (
            NumericOperator::CubeRoot,
            vec![Some(ColumnType::DoublePrecision)],
            ColumnType::DoublePrecision,
            597,
            231,
        ),
    ];
    for (operator, arguments, result, oid, function_oid) in cases {
        let selected = numeric_operator_types_with_control(operator, &arguments, &control).unwrap();
        assert_eq!(selected.arguments.len(), arguments.len());
        assert_eq!(selected.result, result);
        assert_eq!(selected.oid, oid);
        assert_eq!(selected.function_oid, function_oid);
        assert_eq!(
            selected.reserved_bytes(),
            selected.arguments.capacity() * size_of::<ColumnType>()
        );
        assert_eq!(budget.used(), selected.reserved_bytes());
        drop(selected);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn numeric_type_workspace_failures_do_not_become_operator_resolution_errors() {
    for limit in [0, 32, 128, 512, 4096, 65536] {
        let budget = MemoryBudget::new(limit);
        let cancellation = CancellationToken::new();
        match numeric_operator_types_with_control(
            NumericOperator::Plus,
            &[Some(ColumnType::Integer)],
            &ProductionControl::new(&budget, &cancellation, &cancellation),
        ) {
            Ok(selected) => {
                assert_eq!(selected.result, ColumnType::Integer);
                drop(selected);
            }
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let error = numeric_operator_types_with_control(
        NumericOperator::Plus,
        &[Some(ColumnType::Boolean)],
        &ProductionControl::new(&budget, &cancellation, &cancellation),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn either_active_owner_cancels_numeric_type_production() {
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = numeric_operator_types_with_control(
            NumericOperator::Plus,
            &[Some(ColumnType::Integer)],
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}
