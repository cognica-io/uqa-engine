//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, ArrayValue, CancellationToken, DecimalValue, TemporalValue};

#[test]
fn legacy_vector_errors_follow_array_shape_null_and_element_short_circuit() {
    use uqa_core::{LegacyVectorKind, LegacyVectorValue};
    let invalid = Value::LegacyVector(
        LegacyVectorValue::try_from_array(
            LegacyVectorKind::Oid,
            ArrayValue::try_new(Vec::new()).unwrap(),
        )
        .unwrap(),
    );
    let valid = |n| {
        Value::LegacyVector(
            LegacyVectorValue::try_new(LegacyVectorKind::Oid, vec![Value::Int(n)]).unwrap(),
        )
    };
    let array = |values| Value::Array(ArrayValue::try_new(values).unwrap());
    let left = array(vec![invalid.clone()]);
    let larger = array(vec![invalid.clone(), valid(2)]);
    let budget = MemoryBudget::new(4096);
    let cancel = CancellationToken::new();
    for control in [
        ProductionControl::uncontrolled(),
        ProductionControl::new(&budget, &cancel, &cancel),
    ] {
        assert!(!values_equal_with_control(&left, &larger, &control).unwrap());
        assert_eq!(
            compare_with_control(&left, &larger, &control)
                .unwrap_err()
                .sqlstate(),
            Some("42804")
        );
        assert_eq!(
            values_equal_with_control(&left, &left, &control)
                .unwrap_err()
                .sqlstate(),
            Some("42804")
        );
        assert!(!values_equal_with_control(&array(vec![Value::Null]), &left, &control).unwrap());
        let first = array(vec![valid(1), invalid.clone()]);
        let second = array(vec![valid(2), invalid.clone()]);
        assert!(!values_equal_with_control(&first, &second, &control).unwrap());
        assert!(compare_with_control(&first, &second, &control)
            .unwrap()
            .is_lt());
        let record = Value::Record(vec![("v".into(), invalid.clone())]);
        assert_eq!(
            values_equal_with_control(&record, &record, &control)
                .unwrap_err()
                .sqlstate(),
            Some("42804")
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn numeric_comparison_coercion_errors_propagate_from_ordinary_equality() {
    let huge = Value::Decimal(DecimalValue::parse("1e400").unwrap());
    let float = Value::Float(f64::INFINITY);
    assert_eq!(
        values_equal(&huge, &float).unwrap_err().sqlstate(),
        Some("22003")
    );
    assert_eq!(
        values_equal_nullable(&float, &huge).unwrap_err().sqlstate(),
        Some("22003")
    );
}

#[test]
fn scalar_comparisons_preserve_sql_coercion_and_release_scratch() {
    let budget = MemoryBudget::new(64 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let decimal = Value::Decimal(DecimalValue::parse("1.00").unwrap());
    let month = Value::Temporal(TemporalValue::parse_interval("1 mon").unwrap());
    for (left, right) in [
        (Value::Int(1), decimal.clone()),
        (Value::Float(1.0), decimal.clone()),
        (Value::Bool(true), decimal),
        (Value::FixedChar("a  ".into()), Value::Str("a ".into())),
        (month, Value::Str("30 days".into())),
        (
            Value::JsonB("{\"x\":1.0}".into()),
            Value::JsonB("{\"x\":1}".into()),
        ),
    ] {
        assert!(values_equal_with_control(&left, &right, &control).unwrap());
        assert!(values_equal_with_control(&right, &left, &control).unwrap());
        assert_eq!(
            compare_nullable_with_control(&left, &right, &control).unwrap(),
            Some(Ordering::Equal)
        );
        assert_eq!(budget.used(), 0);
    }
    let date = Value::Temporal(TemporalValue::parse_date("2026-01-02").unwrap());
    let invalid = Value::Str("not a date".into());
    assert!(!values_equal_with_control(&date, &invalid, &control).unwrap());
    assert!(matches!(
        compare_nullable_with_control(&date, &invalid, &control),
        Err(SQLError::TypeMismatch(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn row_unknowns_and_total_container_equality_keep_distinct_semantics() {
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let row = Value::Row(vec![Value::Null, Value::Int(1)]);
    assert_eq!(
        values_equal_nullable_with_control(&row, &row, &control).unwrap(),
        None
    );
    let mismatch = Value::Row(vec![Value::Null, Value::Int(2)]);
    assert_eq!(
        values_equal_nullable_with_control(&row, &mismatch, &control).unwrap(),
        Some(false)
    );
    assert_eq!(
        compare_nullable_with_control(&row, &mismatch, &control).unwrap(),
        None
    );
    for value in [
        Value::Array(ArrayValue::try_new(vec![Value::Null, Value::Int(1)]).unwrap()),
        Value::List(vec![Value::Null, Value::Int(1)]),
        Value::Record(vec![("x".into(), Value::Null), ("y".into(), Value::Int(1))]),
    ] {
        assert_eq!(
            values_equal_nullable_with_control(&value, &value, &control).unwrap(),
            Some(true)
        );
        assert_eq!(
            compare_nullable_with_control(&value, &value, &control).unwrap(),
            Some(Ordering::Equal)
        );
    }
    assert_eq!(budget.used(), 0);
}

#[test]
fn scalar_fast_paths_and_row_short_circuit_do_not_allocate() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let decimal = Value::Decimal(DecimalValue::parse("123.45").unwrap());
    let left = Value::Row(vec![Value::Int(1), decimal.clone()]);
    let right = Value::Row(vec![Value::Int(2), decimal]);
    assert_eq!(
        values_equal_nullable_with_control(&left, &right, &control).unwrap(),
        Some(false)
    );
    assert_eq!(
        compare_nullable_with_control(&left, &right, &control).unwrap(),
        Some(Ordering::Less)
    );
    let left = Value::FixedChar(format!("{}  ", "x".repeat(10_000)));
    let right = Value::Str(format!("{} ", "x".repeat(10_000)));
    assert!(values_equal_with_control(&left, &right, &control).unwrap());
    assert_eq!(
        eval_comparison_truth_with_control(BinaryOp::Equal, &Value::Null, &Value::Null, &control)
            .unwrap(),
        None
    );
    assert_eq!(budget.peak(), 0);
}

#[test]
fn comparison_resource_errors_are_not_converted_to_false_or_unknown() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let decimal = Value::Decimal(DecimalValue::parse("1.0").unwrap());
    for (left, right) in [
        (Value::Int(1), decimal.clone()),
        (
            Value::Row(vec![Value::Int(1), Value::Null]),
            Value::Row(vec![decimal, Value::Null]),
        ),
        (
            Value::Temporal(TemporalValue::parse_interval("1 mon").unwrap()),
            Value::Str("30 days".into()),
        ),
    ] {
        assert_eq!(
            values_equal_with_control(&left, &right, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(
            compare_nullable_with_control(&left, &right, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
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
        let control = ProductionControl::new(&budget, &original, &invoking);
        for operator in [BinaryOp::Equal, BinaryOp::Less] {
            assert_eq!(
                eval_comparison_truth_with_control(operator, &Value::Null, &Value::Null, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
        }
        assert_eq!(
            values_equal_with_control(&Value::Int(1), &Value::Int(1), &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
