//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

#[test]
fn array_element_identity_survives_retention_without_changing_native_equality() {
    let untyped = ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![-3]).unwrap();
    let typed = untyped.clone().with_element_type_oid(Some(23));
    assert_eq!(typed, untyped);
    assert!(!Value::Array(typed.clone()).has_same_representation(&Value::Array(untyped)));
    let encoded = serde_json::to_vec(&typed).unwrap();
    let decoded: ArrayValue = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.element_type_oid(), Some(23));
    let value = Value::Array(typed.clone());
    let encoded = serde_json::to_vec(&value).unwrap();
    let decoded: Value = serde_json::from_slice(&encoded).unwrap();
    assert!(value.has_same_representation(&decoded));

    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let copied = value.clone_budgeted(&memory, &token).unwrap();
    assert!(value.has_same_representation(&copied));
    drop(copied);
    assert_eq!(memory.used(), 0);
    let control = ProductionControl::new(&memory, &token, &token);
    let changed = typed
        .assign_element_with_control(&[-2], &Value::Int(2), &control)
        .unwrap();
    assert_eq!(changed.element_type_oid(), Some(23));
    assert_eq!(changed.elements(), &[Value::Int(1), Value::Int(2)]);
    assert_eq!(changed.lower_bounds(), &[-3]);
    drop(changed);
    assert_eq!(memory.used(), 0);
}

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
