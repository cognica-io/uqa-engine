//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

#[test]
fn produced_arrays_preserve_bounds_and_release_nested_headers() {
    let source = Value::List(vec![Value::Array(
        ArrayValue::with_lower_bounds(vec![Value::Str("owned".into())], vec![-5]).unwrap(),
    )]);
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let (Value::List(elements), memory) = control.copy_value(&source).unwrap().into_parts() else {
        unreachable!();
    };
    let elements = control.finish(elements, memory).unwrap();
    let mut lower = ProductionVec::new(control);
    lower.push_copy(-2).unwrap();
    lower.push_copy(4).unwrap();
    let array =
        ArrayValue::with_lower_bounds_with_control(elements, lower.finish().unwrap(), &control)
            .unwrap()
            .unwrap();
    assert_eq!(array.dimensions(), &[1, 1]);
    assert_eq!(array.lower_bounds(), &[-2, 4]);
    assert!(matches!(array.elements()[0], Value::List(_)));
    let exact = array.retained_buffer_bytes().unwrap() + size_of::<Value>() + "owned".len();
    assert_eq!(array.reserved_bytes(), exact);
    assert_eq!(budget.used(), exact);
    drop(array);
    assert_eq!(budget.used(), 0);
}

#[test]
fn produced_array_failures_release_inputs_and_shape_workspace() {
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut elements = ProductionVec::new(control);
        elements
            .push_produced(control.copy_value(&Value::Str("held".into())).unwrap())
            .unwrap();
        let elements = elements.finish().unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            ArrayValue::try_new_with_control(elements, &control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let elements = ProductionVec::new(control).finish().unwrap();
    assert!(matches!(
        ArrayValue::try_new_with_control(elements, &control),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(budget.used(), 0);
}
