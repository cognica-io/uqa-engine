//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken, LegacyVectorKind, LegacyVectorValue};

fn array(values: &[i64]) -> ArrayValue {
    ArrayValue::try_new(values.iter().copied().map(Value::Int).collect()).unwrap()
}

#[test]
fn element_replacement_preserves_other_values_and_extends_with_nulls() {
    let original = array(&[1, 2, 3]);
    let control = ProductionControl::uncontrolled();
    for (index, expected, lower) in [
        (2, vec![Value::Int(1), Value::Int(9), Value::Int(3)], 1),
        (
            -1,
            vec![
                Value::Int(9),
                Value::Null,
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
            ],
            -1,
        ),
        (
            5,
            vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Null,
                Value::Int(9),
            ],
            1,
        ),
    ] {
        let result = original
            .assign_element_with_control(&[index], &Value::Int(9), &control)
            .unwrap();
        assert_eq!(result.elements(), expected);
        assert_eq!(result.lower_bounds(), &[lower]);
    }
    let result = original
        .assign_element_with_control(&[2], &Value::Null, &control)
        .unwrap();
    assert_eq!(
        result.elements(),
        &[Value::Int(1), Value::Null, Value::Int(3)]
    );
    assert_eq!(original, array(&[1, 2, 3]));
}

#[test]
fn slices_use_flat_source_values_and_fill_omitted_destination_dimensions() {
    let control = ProductionControl::uncontrolled();
    let source = array(&[8, 9, 10]);
    let original = array(&[1, 2, 3]);
    for (bounds, expected) in [
        ((Some(2), Some(3)), [1, 8, 9]),
        ((None, Some(2)), [8, 9, 3]),
        ((Some(2), None), [1, 8, 9]),
    ] {
        let result = original
            .assign_slice_with_control(&[bounds], &source, &control)
            .unwrap();
        assert_eq!(&*result, &array(&expected));
    }
    let original = ArrayValue::with_lower_bounds(
        vec![
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(3), Value::Int(4)]),
        ],
        vec![0, -1],
    )
    .unwrap();
    let result = original
        .assign_slice_with_control(&[(Some(1), Some(1))], &source, &control)
        .unwrap();
    assert_eq!(result.lower_bounds(), &[0, -1]);
    assert_eq!(
        result.elements(),
        &[
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(8), Value::Int(9)]),
        ]
    );
    let result = original
        .assign_slice_with_control(&[(Some(0), Some(1)), (Some(0), Some(0))], &source, &control)
        .unwrap();
    assert_eq!(
        result.elements(),
        &[
            Value::List(vec![Value::Int(1), Value::Int(8)]),
            Value::List(vec![Value::Int(3), Value::Int(9)]),
        ]
    );
    let result = original
        .assign_element_with_control(&[1, 0], &Value::Int(9), &control)
        .unwrap();
    assert_eq!(
        result.elements()[1],
        Value::List(vec![Value::Int(3), Value::Int(9)])
    );
    assert!(matches!(
        original.assign_element_with_control(&[2, 0], &Value::Int(9), &control),
        Err(ArrayAssignmentError::SubscriptRange)
    ));
    assert!(matches!(
        original.assign_element_with_control(&[1], &Value::Int(9), &control),
        Err(ArrayAssignmentError::SubscriptCount)
    ));
}

#[test]
fn empty_destinations_acquire_explicit_bounds_and_keep_error_precedence() {
    let control = ProductionControl::uncontrolled();
    for original in [
        array(&[]),
        ArrayValue::with_lower_bounds(Vec::new(), vec![0]).unwrap(),
    ] {
        let result = original
            .assign_element_with_control(&[-2, 4], &Value::Int(9), &control)
            .unwrap();
        assert_eq!(result.dimensions(), &[1, 1]);
        assert_eq!(result.lower_bounds(), &[-2, 4]);
        assert_eq!(result.elements(), &[Value::List(vec![Value::Int(9)])]);
        assert!(matches!(
            original.assign_slice_with_control(&[(None, Some(2))], &array(&[1, 2]), &control),
            Err(ArrayAssignmentError::MissingBounds)
        ));
        let result = original
            .assign_slice_with_control(&[(Some(3), Some(2))], &array(&[1]), &control)
            .unwrap();
        assert!(result.dimensions().is_empty());
        assert!(matches!(
            original.assign_slice_with_control(&[(Some(3), Some(1))], &array(&[1]), &control),
            Err(ArrayAssignmentError::SizeLimit)
        ));
        assert!(matches!(
            original.assign_slice_with_control(
                &[(Some(i32::MAX), Some(i32::MAX))],
                &array(&[]),
                &control
            ),
            Err(ArrayAssignmentError::SourceTooSmall)
        ));
        assert!(matches!(
            original.assign_element_with_control(&[i32::MAX], &Value::Int(1), &control),
            Err(ArrayAssignmentError::LowerBound(i32::MAX))
        ));
    }
    assert!(matches!(
        array(&[1]).assign_slice_with_control(&[(Some(2), Some(1))], &array(&[1]), &control),
        Err(ArrayAssignmentError::ReversedBounds)
    ));
    assert!(matches!(
        array(&[1]).assign_slice_with_control(&[(Some(1), Some(2))], &array(&[1]), &control),
        Err(ArrayAssignmentError::SourceTooSmall)
    ));
    let original = ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![i32::MAX - 1]).unwrap();
    assert!(matches!(
        original.assign_slice_with_control(
            &[(Some(i32::MAX), Some(i32::MAX))],
            &array(&[]),
            &control,
        ),
        Err(ArrayAssignmentError::LowerBound(2_147_483_646))
    ));
}

#[test]
fn replacement_retains_vector_elements_and_releases_memory_on_success_and_failure() {
    let vector = Value::LegacyVector(
        LegacyVectorValue::try_new(LegacyVectorKind::Oid, vec![Value::Int(7)]).unwrap(),
    );
    let source = ArrayValue::try_new(vec![vector.clone(), Value::Null]).unwrap();
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let result = source
        .assign_element_with_control(&[2], &vector, &control)
        .unwrap();
    assert_eq!(result.dimensions(), &[2]);
    assert_eq!(result.elements(), &[vector.clone(), vector]);
    assert_eq!(result.reserved_bytes(), budget.used());
    assert!(budget.used() > 0);
    drop(result);
    assert_eq!(budget.used(), 0);
    assert!(matches!(
        source.assign_element_with_control(&[1000], &Value::Int(1), &control),
        Err(ArrayAssignmentError::Retention(
            ValueRetentionError::Memory(_)
        ))
    ));
    assert_eq!(budget.used(), 0);
    cancellation.cancel();
    assert!(matches!(
        source.assign_element_with_control(&[2], &Value::Null, &control),
        Err(ArrayAssignmentError::Retention(
            ValueRetentionError::Cancelled(_)
        ))
    ));
    assert_eq!(budget.used(), 0);
    assert!(matches!(source.elements()[1], Value::Null));
}
