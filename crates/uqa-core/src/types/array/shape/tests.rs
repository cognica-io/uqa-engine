//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryError, ArrayValue};

#[test]
fn shape_validation_preserves_empty_dimensions_and_rejects_mixed_or_ragged_values() {
    let cases = [
        (vec![], Some(vec![])),
        (vec![Value::Null, Value::Int(1)], Some(vec![2])),
        (
            vec![Value::List(vec![]), Value::List(vec![])],
            Some(vec![2, 0]),
        ),
        (
            vec![
                Value::Array(ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![-4]).unwrap()),
                Value::List(vec![Value::Null]),
            ],
            Some(vec![2, 1]),
        ),
        (
            vec![Value::List(vec![]), Value::List(vec![Value::Int(1)])],
            None,
        ),
        (vec![Value::List(vec![]), Value::Int(1)], None),
        (vec![Value::Int(1), Value::List(vec![])], None),
        (
            vec![
                Value::List(vec![Value::List(vec![])]),
                Value::List(vec![Value::Int(1)]),
            ],
            None,
        ),
    ];
    for (values, expected) in cases {
        let memory = MemoryBudget::new(1 << 20);
        assert_eq!(unbounded(&values), expected);
        let dimensions = budgeted(&values, &memory, &CancellationToken::new()).unwrap();
        assert_eq!(dimensions.as_deref(), expected.as_ref());
        if let Some(dimensions) = dimensions {
            assert_eq!(
                dimensions.reserved_bytes(),
                dimensions.capacity() * size_of::<usize>()
            );
            assert_eq!(memory.used(), dimensions.reserved_bytes());
            drop(dimensions);
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn shape_failures_keep_existing_results_and_release_partial_workspace() {
    let cancellation = CancellationToken::new();
    let memory = MemoryBudget::new(64);
    let previous = memory.reserve(17).unwrap();
    let mut nested = Value::Null;
    for _ in 0..512 {
        nested = Value::List(vec![nested]);
    }
    assert!(matches!(
        budgeted(std::slice::from_ref(&nested), &memory, &cancellation),
        Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(memory.used(), previous.bytes());
    cancellation.cancel();
    assert!(matches!(
        budgeted(std::slice::from_ref(&nested), &memory, &cancellation),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(memory.used(), previous.bytes());
    while let Value::List(mut values) = nested {
        nested = values.pop().unwrap();
    }
}

#[test]
fn deep_shape_validation_uses_charged_iterators_instead_of_recursive_calls() {
    let mut nested = Value::Null;
    for _ in 0..512 {
        nested = Value::List(vec![nested]);
    }
    let memory = MemoryBudget::new(1 << 20);
    let dimensions = budgeted(
        std::slice::from_ref(&nested),
        &memory,
        &CancellationToken::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(dimensions.len(), 513);
    assert!(dimensions.iter().all(|dimension| *dimension == 1));
    assert_eq!(memory.used(), dimensions.reserved_bytes());
    assert!(memory.peak() > memory.used());
    drop(dimensions);
    assert_eq!(memory.used(), 0);
    while let Value::List(mut values) = nested {
        nested = values.pop().unwrap();
    }
}
