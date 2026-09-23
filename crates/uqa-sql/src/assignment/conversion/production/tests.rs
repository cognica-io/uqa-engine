//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_assignment_preserves_character_array_numeric_and_vector_results() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let cases = [
        (
            Value::Str("é  ".into()),
            ColumnType::Varchar(Some(1)),
            Value::Str("é".into()),
        ),
        (
            Value::Str("é".into()),
            ColumnType::Character(3),
            Value::FixedChar("é  ".into()),
        ),
        (
            Value::Str("5".into()),
            ColumnType::Domain {
                schema: "public".into(),
                name: "number".into(),
                oid: 12,
                base: Box::new(ColumnType::Integer),
            },
            Value::Int(5),
        ),
        (
            Value::Str("-12.345".into()),
            ColumnType::Numeric {
                precision: Some(4),
                scale: Some(2),
            },
            Value::Decimal(DecimalValue::parse("-12.35").unwrap()),
        ),
        (
            Value::List(vec![Value::Int(1), Value::Float(2.5)]),
            ColumnType::Vector(2),
            Value::List(vec![Value::Float(1.0), Value::Float(2.5)]),
        ),
        (
            Value::Array(
                ArrayValue::with_lower_bounds(
                    vec![Value::Str("2".into()), Value::Str("3".into())],
                    vec![-2],
                )
                .unwrap(),
            ),
            ColumnType::Array(Box::new(ColumnType::Integer)),
            Value::Array(
                ArrayValue::with_lower_bounds(vec![Value::Int(2), Value::Int(3)], vec![-2])
                    .unwrap(),
            ),
        ),
    ];
    for (source, ty, expected) in cases {
        let source = control.copy_value(&source).unwrap();
        let result = convert_value_to_column_type_with_control(source, &ty, &control).unwrap();
        assert_eq!(&*result, &expected);
        assert_eq!(budget.used(), result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
    let invalid = control.copy_value(&Value::Str("long".into())).unwrap();
    let error =
        convert_value_to_column_type_with_control(invalid, &ColumnType::Varchar(Some(1)), &control)
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22001"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn record_assignment_moves_existing_payloads_and_drops_the_source_buffer() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let source = control
        .copy_value(&Value::Row(vec![Value::Str("moved".into()), Value::Int(7)]))
        .unwrap();
    let Value::Row(source_values) = &*source else {
        unreachable!();
    };
    let Value::Str(source_text) = &source_values[0] else {
        unreachable!();
    };
    let pointer = source_text.as_ptr();
    let result =
        convert_value_to_column_type_with_control(source, &ColumnType::Record, &control).unwrap();
    let Value::Record(fields) = &*result else {
        unreachable!();
    };
    let Value::Str(text) = &fields[0].1 else {
        unreachable!();
    };
    assert_eq!(text.as_ptr(), pointer);
    assert_eq!(fields[0].0, "f1");
    assert_eq!(fields[1], ("f2".into(), Value::Int(7)));
    let exact = fields.capacity() * size_of::<(String, Value)>()
        + fields
            .iter()
            .map(|(name, _)| name.capacity())
            .sum::<usize>()
        + text.capacity();
    assert_eq!(result.reserved_bytes(), exact);
    assert_eq!(budget.used(), exact);
    drop(result);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_assignment_releases_partial_values_and_preserves_cancellation_kind() {
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let source = control.copy_value(&Value::Str("held".into())).unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error =
            convert_value_to_column_type_with_control(source, &ColumnType::Character(20), &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(128);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let source = control.copy_value(&Value::Str("held".into())).unwrap();
    let error =
        convert_value_to_column_type_with_control(source, &ColumnType::Character(4096), &control)
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
}
