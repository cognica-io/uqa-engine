//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, DecimalValue, TemporalValue};

#[test]
fn binary_inline_results_preserve_width_null_and_error_behavior_at_zero_budget() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    for (operation, left, right, expected) in [
        (BinaryOp::Add, Value::Int(1), Value::Int(2), Value::Int(3)),
        (
            BinaryOp::Divide,
            Value::Int(-7),
            Value::Int(2),
            Value::Int(-3),
        ),
        (
            BinaryOp::Less,
            Value::Int(1),
            Value::Int(2),
            Value::Bool(true),
        ),
        (BinaryOp::Multiply, Value::Null, Value::Int(2), Value::Null),
        (
            BinaryOp::Add,
            Value::Float(1.5),
            Value::Int(2),
            Value::Float(3.5),
        ),
    ] {
        let value = eval_binary_values_with_control(operation, &left, &right, &control).unwrap();
        assert_eq!(*value, expected);
        assert_eq!(value.reserved_bytes(), 0);
    }
    for (operation, left, right, expected) in [
        (BinaryOp::Divide, i64::MIN, -1, "22003"),
        (BinaryOp::Divide, 1, 0, "22012"),
        (BinaryOp::Add, i64::MAX, 1, "22003"),
    ] {
        assert_eq!(
            eval_binary_values_with_control(
                operation,
                &Value::Int(left),
                &Value::Int(right),
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some(expected)
        );
    }
    assert_eq!(
        eval_binary_values_with_integer_width_with_control(
            BinaryOp::Add,
            &Value::Int(i64::from(i16::MAX)),
            &Value::Int(1),
            Some(IntegerWidth::SmallInt),
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("22003")
    );
    assert_eq!(
        integer_width_for_type(" PG_CATALOG.INT4 "),
        Some(IntegerWidth::Integer)
    );
    assert_eq!(budget.peak(), 0);
}

#[test]
fn decimal_binary_results_retain_their_owner_and_release_temporary_coercions() {
    let budget = MemoryBudget::new(256 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let left = Value::Decimal(DecimalValue::parse("7.50").unwrap());
    let right = Value::Int(2);
    for (operation, expected) in [
        (BinaryOp::Add, "9.5"),
        (BinaryOp::Subtract, "5.5"),
        (BinaryOp::Multiply, "15"),
        (BinaryOp::Divide, "3.75"),
    ] {
        let value = eval_binary_values_with_control(operation, &left, &right, &control).unwrap();
        assert_eq!(
            *value,
            Value::Decimal(DecimalValue::parse(expected).unwrap())
        );
        assert!(value.reserved_bytes() > 0);
        assert_eq!(budget.used(), value.reserved_bytes());
        drop(value);
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(
        eval_binary_values_with_control(BinaryOp::Divide, &left, &Value::Int(0), &control)
            .unwrap_err()
            .sqlstate(),
        Some("22012")
    );
    assert_eq!(budget.used(), 0);
    let mixed = eval_binary_values_with_control(BinaryOp::Add, &left, &Value::Float(0.5), &control)
        .unwrap();
    assert_eq!(*mixed, Value::Float(8.0));
    assert_eq!(mixed.reserved_bytes(), 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn binary_json_and_temporal_operators_use_existing_controlled_owners() {
    let budget = MemoryBudget::new(128 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let json = Value::JsonB("{\"x\":1,\"y\":2}".into());
    let result = eval_binary_values_with_control(
        BinaryOp::Subtract,
        &json,
        &Value::Str("x".into()),
        &control,
    )
    .unwrap();
    assert_eq!(*result, Value::JsonB("{\"y\":2}".into()));
    assert!(result.reserved_bytes() > 0);
    assert_eq!(budget.used(), result.reserved_bytes());
    drop(result);
    let date = Value::Temporal(TemporalValue::parse_date("2026-01-01").unwrap());
    let result =
        eval_binary_values_with_control(BinaryOp::Add, &date, &Value::Int(2), &control).unwrap();
    assert_eq!(
        *result,
        Value::Temporal(TemporalValue::parse_date("2026-01-03").unwrap())
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn binary_production_propagates_quota_and_both_cancellation_sources() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let decimal = Value::Decimal(DecimalValue::parse("1.25").unwrap());
    assert_eq!(
        eval_binary_values_with_control(BinaryOp::Add, &decimal, &Value::Int(1), &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(
        super::super::eval_float_arithmetic_with_control(
            BinaryOp::Add,
            &decimal,
            &Value::Float(1.0),
            super::super::FloatWidth::Real,
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
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
            eval_binary_values_with_control(BinaryOp::Add, &Value::Null, &Value::Null, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            super::super::eval_float_arithmetic_with_control(
                BinaryOp::Multiply,
                &Value::Int(1),
                &Value::Int(2),
                super::super::FloatWidth::Real,
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
