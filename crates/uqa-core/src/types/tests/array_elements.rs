//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tracked traversal preserves the native array comparator's row-major stream.

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

#[test]
fn bounded_array_elements_preserve_shape_nulls_and_empty_dimensions() {
    for (elements, expected) in [
        (vec![], vec![]),
        (vec![Value::List(vec![]), Value::List(vec![])], vec![]),
        (
            vec![Value::Int(1), Value::Null],
            vec![Value::Int(1), Value::Null],
        ),
        (
            vec![
                Value::List(vec![Value::Int(1), Value::Null]),
                Value::List(vec![Value::Int(2), Value::Int(3)]),
            ],
            vec![Value::Int(1), Value::Null, Value::Int(2), Value::Int(3)],
        ),
    ] {
        let array = ArrayValue::try_new(elements).unwrap();
        let memory = MemoryBudget::new(1024);
        let cancellation = CancellationToken::new();
        let mut cursor = array.budgeted_elements(&memory, &cancellation).unwrap();
        let mut actual = Vec::new();
        while let Some(value) = cursor.next_element().unwrap() {
            actual.push(value.clone());
        }
        assert_eq!(actual, expected);
        assert_eq!(
            array.flattened_elements().cloned().collect::<Vec<_>>(),
            expected
        );
        drop(cursor);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn bounded_array_elements_charge_depth_and_honor_cancellation_between_items() {
    let mut nested = Value::Int(1);
    for _ in 0..32 {
        nested = Value::List(vec![nested]);
    }
    let array = ArrayValue::try_new(vec![nested]).unwrap();
    let memory = MemoryBudget::new(128);
    let cancellation = CancellationToken::new();
    let mut cursor = array.budgeted_elements(&memory, &cancellation).unwrap();
    assert!(matches!(
        cursor.next_element(),
        Err(ArrayTraversalError::Memory(_))
    ));
    drop(cursor);
    assert_eq!(memory.used(), 0);

    let array = ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap();
    let mut cursor = array.budgeted_elements(&memory, &cancellation).unwrap();
    assert_eq!(cursor.next_element().unwrap(), Some(&Value::Int(1)));
    cancellation.cancel();
    assert!(matches!(
        cursor.next_element(),
        Err(ArrayTraversalError::Cancelled(_))
    ));
    drop(cursor);
    assert_eq!(memory.used(), 0);
    assert!(matches!(
        array.budgeted_elements(&memory, &cancellation),
        Err(ArrayTraversalError::Cancelled(_))
    ));
}
