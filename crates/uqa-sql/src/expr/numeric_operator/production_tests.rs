//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, DecimalValue};

#[test]
fn numeric_operator_producers_preserve_selected_values_widths_and_error_states() {
    let budget = MemoryBudget::new(1 << 18);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (operator, values, types, expected) in [
        (
            NumericOperator::Modulo,
            vec![Value::Int(7), Value::Int(3)],
            vec![Some(ColumnType::SmallInteger); 2],
            Value::Int(1),
        ),
        (
            NumericOperator::Power,
            vec![Value::Int(2), Value::Int(3)],
            vec![Some(ColumnType::Integer); 2],
            Value::Float(8.0),
        ),
        (
            NumericOperator::Plus,
            vec![Value::Int(17)],
            vec![Some(ColumnType::Integer)],
            Value::Int(17),
        ),
        (
            NumericOperator::SquareRoot,
            vec![Value::Int(4)],
            vec![Some(ColumnType::Integer)],
            Value::Float(2.0),
        ),
        (
            NumericOperator::CubeRoot,
            vec![Value::Int(8)],
            vec![Some(ColumnType::Integer)],
            Value::Float(2.0),
        ),
        (
            NumericOperator::Absolute,
            vec![Value::Int(-8)],
            vec![Some(ColumnType::Integer)],
            Value::Int(8),
        ),
        (
            NumericOperator::Modulo,
            vec![Value::Null, Value::Int(3)],
            vec![Some(ColumnType::Integer); 2],
            Value::Null,
        ),
    ] {
        let result =
            eval_numeric_operator_with_control(operator, &values, &types, &control).unwrap();
        assert_eq!(*result, expected);
        drop(result);
        assert_eq!(budget.used(), 0);
    }
    for (operator, values, types, state) in [
        (
            NumericOperator::Absolute,
            vec![Value::Int(i64::from(i16::MIN))],
            vec![Some(ColumnType::SmallInteger)],
            "22003",
        ),
        (
            NumericOperator::Modulo,
            vec![Value::Int(4), Value::Int(0)],
            vec![Some(ColumnType::Integer); 2],
            "22012",
        ),
    ] {
        let error =
            eval_numeric_operator_with_control(operator, &values, &types, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn bound_decimal_operators_keep_results_charged_after_cast_scratch_drops() {
    let budget = MemoryBudget::new(1 << 18);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut binding = FunctionBinding::dispatched(crate::ast::FunctionDispatch::NumericOperator(
        NumericOperator::Modulo,
    ));
    binding.argument_types = vec!["numeric".into(), "numeric".into()];
    let result = eval_bound_operator_with_control(
        NumericOperator::Modulo,
        &binding,
        &[
            Value::Decimal(DecimalValue::parse("7.5").unwrap()),
            Value::Int(2),
        ],
        &control,
    )
    .unwrap();
    assert_eq!(*result, Value::Decimal(DecimalValue::parse("1.5").unwrap()));
    assert!(result.reserved_bytes() > 0);
    assert_eq!(budget.used(), result.reserved_bytes());
    drop(result);
    assert_eq!(budget.used(), 0);
}

#[test]
fn numeric_operator_quota_and_both_cancellations_preserve_existing_owners() {
    let budget = MemoryBudget::new(128);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("live").unwrap();
    let occupied = budget.reserve(budget.limit() - budget.used()).unwrap();
    let run = || {
        eval_numeric_operator_with_control(
            NumericOperator::Plus,
            &[Value::Int(5)],
            &[Some(ColumnType::Integer)],
            &control,
        )
    };
    assert_eq!(run().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(&*held, "live");
    assert_eq!(budget.used(), held.reserved_bytes() + occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(run().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(budget.used(), held.reserved_bytes());
        token.reset();
    }
    drop(held);
    assert_eq!(budget.used(), 0);
}
