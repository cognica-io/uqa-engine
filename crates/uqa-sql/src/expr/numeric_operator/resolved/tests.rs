//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, DecimalValue};

fn resolve(
    operator: NumericOperator,
    values: &[Value],
    types: &[Option<ColumnType>],
) -> ResolvedNumericOperator {
    super::super::eval_numeric_operator_with_resolution(
        operator,
        values,
        types,
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
    .1
    .unwrap()
}

#[test]
fn resolved_numeric_operands_convert_before_null_and_preserve_result_widths() {
    let control = ProductionControl::uncontrolled();
    let modulo = resolve(
        NumericOperator::Modulo,
        &[Value::Int(7), Value::Int(3)],
        &[Some(ColumnType::Integer), Some(ColumnType::Integer)],
    );
    assert_eq!(
        *modulo
            .evaluate(&[Value::Null, Value::Int(3)], &control)
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        modulo
            .evaluate(&[Value::Null, Value::Int(i64::MAX)], &control)
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    assert_eq!(
        modulo
            .evaluate(&[Value::Int(7), Value::Int(0)], &control)
            .unwrap_err()
            .sqlstate(),
        Some("22012")
    );
    for ty in [ColumnType::SmallInteger, ColumnType::Integer] {
        let minimum = if ty == ColumnType::SmallInteger {
            i64::from(i16::MIN)
        } else {
            i64::from(i32::MIN)
        };
        let absolute = resolve(NumericOperator::Absolute, &[Value::Int(-7)], &[Some(ty)]);
        assert_eq!(
            *absolute.evaluate(&[Value::Int(-7)], &control).unwrap(),
            Value::Int(7)
        );
        assert_eq!(
            absolute
                .evaluate(&[Value::Int(minimum)], &control)
                .unwrap_err()
                .sqlstate(),
            Some("22003")
        );
    }
    let real = resolve(
        NumericOperator::Plus,
        &[Value::Float(1.25)],
        &[Some(ColumnType::Real)],
    );
    assert_eq!(
        *real
            .evaluate(&[Value::Float(16_777_217.0)], &control)
            .unwrap(),
        Value::Float(16_777_216.0)
    );
}

#[test]
fn resolved_decimal_operators_charge_results_to_the_invoking_owner() {
    let decimal = |value| Value::Decimal(DecimalValue::parse(value).unwrap());
    let operation = resolve(
        NumericOperator::Modulo,
        &[decimal("7.5"), Value::Int(2)],
        &[
            Some(ColumnType::Numeric {
                precision: None,
                scale: None,
            }),
            Some(ColumnType::Integer),
        ],
    );
    let budget = MemoryBudget::new(64 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let value = operation
        .evaluate(&[decimal("7.5"), Value::Int(2)], &control)
        .unwrap();
    assert_eq!(*value, decimal("1.5"));
    assert!(value.reserved_bytes() > 0);
    assert_eq!(budget.used(), value.reserved_bytes());
    let occupied = budget.reserve(budget.limit() - budget.used()).unwrap();
    assert_eq!(
        operation
            .evaluate(&[Value::Int(7), Value::Int(2)], &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), value.reserved_bytes() + occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            operation
                .evaluate(&[Value::Int(7), Value::Int(2)], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), value.reserved_bytes());
        token.reset();
    }
    drop(value);
    assert_eq!(budget.used(), 0);
}

#[test]
fn unknown_operator_operands_keep_the_unresolved_conversion_path() {
    let (value, selected) = super::super::eval_numeric_operator_with_resolution(
        NumericOperator::Modulo,
        &[Value::Str("7".into()), Value::Int(3)],
        &[None, Some(ColumnType::Integer)],
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    assert_eq!(*value, Value::Int(1));
    assert!(selected.is_none());
    let (value, selected) = super::super::eval_numeric_operator_with_resolution(
        NumericOperator::Modulo,
        &[Value::Null, Value::Int(3)],
        &[None, Some(ColumnType::Integer)],
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    assert_eq!(*value, Value::Null);
    assert!(selected.is_none());
}
