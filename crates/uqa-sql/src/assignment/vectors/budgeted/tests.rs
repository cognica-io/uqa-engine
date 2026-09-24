//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::assignment::vectors::index_vectors_for_type;
use uqa_core::{ArrayValue, DecimalValue};

#[test]
fn converted_vectors_match_assignment_and_keep_all_buffer_capacities_charged() {
    let values = vec![
        Value::Int(-7),
        Value::Float(0.5),
        Value::Decimal(DecimalValue::parse("1.25").unwrap()),
    ];
    let tensor = vec![Value::List(values.clone()), Value::List(values.clone())];
    for (value, ty) in [
        (Value::List(values.clone()), ColumnType::Vector(3)),
        (
            Value::Array(ArrayValue::with_lower_bounds(values, vec![-2]).unwrap()),
            ColumnType::Vector(3),
        ),
        (Value::List(tensor.clone()), ColumnType::Tensor(3)),
        (
            Value::Array(ArrayValue::with_lower_bounds(tensor, vec![-1, 4]).unwrap()),
            ColumnType::Tensor(3),
        ),
        (Value::List(Vec::new()), ColumnType::Tensor(3)),
        (Value::Null, ColumnType::Vector(3)),
    ] {
        let memory = MemoryBudget::new(4096);
        let converted =
            index_vectors_for_type_budgeted(&value, &ty, &memory, &mut || Ok(())).unwrap();
        assert_eq!(*converted, index_vectors_for_type(&value, &ty).unwrap());
        let bytes = converted.capacity() * size_of::<Vec<f32>>()
            + converted
                .iter()
                .map(|vector| vector.capacity() * size_of::<f32>())
                .sum::<usize>();
        assert_eq!(converted.reserved_bytes(), bytes);
        assert_eq!(memory.used(), bytes);
        drop(converted);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn invalid_shapes_and_numeric_elements_preserve_assignment_diagnostics() {
    for (value, ty) in [
        (Value::Int(1), ColumnType::Vector(2)),
        (Value::List(vec![Value::Int(1)]), ColumnType::Vector(2)),
        (
            Value::List(vec![Value::Float(f64::NAN)]),
            ColumnType::Vector(1),
        ),
        (
            Value::List(vec![Value::Float(f64::MAX)]),
            ColumnType::Vector(1),
        ),
        (
            Value::List(vec![Value::Str("1".into())]),
            ColumnType::Vector(1),
        ),
        (Value::List(vec![Value::Int(1)]), ColumnType::Tensor(1)),
        (
            Value::List(vec![
                Value::List(vec![Value::Int(1)]),
                Value::List(vec![Value::Str("invalid".into())]),
            ]),
            ColumnType::Tensor(2),
        ),
        (
            Value::Array(ArrayValue::try_new(vec![Value::List(vec![Value::Int(1)])]).unwrap()),
            ColumnType::Vector(1),
        ),
        (Value::List(Vec::new()), ColumnType::Text),
    ] {
        let memory = MemoryBudget::new(4096);
        let expected = index_vectors_for_type(&value, &ty).unwrap_err();
        let actual =
            index_vectors_for_type_budgeted(&value, &ty, &memory, &mut || Ok(())).unwrap_err();
        assert_eq!(actual.sqlstate(), expected.sqlstate());
        assert_eq!(actual.to_string(), expected.to_string());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn quota_rejection_precedes_float_buffer_allocation_and_leaves_inputs_unchanged() {
    let value = Value::List(vec![Value::Int(1); 1024]);
    let original = value.clone();
    let memory = MemoryBudget::new(size_of::<Vec<f32>>());
    let error =
        index_vectors_for_type_budgeted(&value, &ColumnType::Vector(1024), &memory, &mut || Ok(()))
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(value, original);
    assert_eq!(memory.peak(), size_of::<Vec<f32>>());
    assert_eq!(memory.used(), 0);
}

#[test]
fn cancellation_during_each_tensor_conversion_step_releases_partial_buffers() {
    let value = Value::List(vec![Value::List(vec![Value::Int(1); 32]); 2]);
    for cancel_at in 1..=70 {
        let memory = MemoryBudget::new(4096);
        let mut polls = 0;
        let error =
            index_vectors_for_type_budgeted(&value, &ColumnType::Tensor(32), &memory, &mut || {
                polls += 1;
                if polls == cancel_at {
                    Err(QueryCancelled)
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}
