//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{cmp::Reverse, collections::BinaryHeap};

use proptest::prelude::*;

use super::*;

proptest! {
    #[test]
    fn mixed_push_and_pop_operations_match_the_standard_priority_queue(
        operations in prop::collection::vec(prop::option::of(any::<i64>()), 0..256)
    ) {
        let budget = MemoryBudget::new(1 << 20);
        let mut actual = BudgetedBinaryHeap::new(&budget);
        let mut expected = BinaryHeap::new();
        for operation in operations {
            if let Some(value) = operation {
                actual.push(value).unwrap();
                expected.push(value);
            } else {
                prop_assert_eq!(actual.pop(), expected.pop());
            }
            prop_assert_eq!(actual.peek(), expected.peek());
            prop_assert_eq!(actual.len(), expected.len());
            prop_assert_eq!(actual.is_empty(), expected.is_empty());
        }
        while let Some(expected) = expected.pop() {
            prop_assert_eq!(actual.pop(), Some(expected));
        }
        prop_assert!(actual.pop().is_none());
        drop(actual);
        prop_assert_eq!(budget.used(), 0);
    }
}

#[test]
fn rejected_push_keeps_priority_and_popping_needs_no_allocation() {
    let budget = MemoryBudget::new(8);
    let mut heap = BudgetedBinaryHeap::new(&budget);
    heap.push(4_u64).unwrap();
    assert!(matches!(heap.push(9), Err(MemoryError::Limit { .. })));
    assert_eq!(heap.len(), 1);
    assert_eq!(heap.peek(), Some(&4));
    assert_eq!(heap.pop(), Some(4));
    assert!(heap.is_empty());
    assert_eq!(budget.used(), 8);
    drop(heap);
    assert_eq!(budget.used(), 0);
}

#[test]
fn reversed_priorities_and_owned_buffer_transfer_retain_the_original_allowance() {
    let budget = MemoryBudget::new(4096);
    let mut heap = BudgetedBinaryHeap::new(&budget);
    heap.reserve(4).unwrap();
    for value in ["z", "a", "a", "m"] {
        heap.push(Reverse(value.to_owned())).unwrap();
    }
    assert_eq!(heap.pop().unwrap().0, "a");
    let used = budget.used();
    let mut values = heap.into_vec();
    assert_eq!(budget.used(), used);
    values.sort_unstable();
    assert_eq!(
        values
            .iter()
            .map(|value| value.0.as_str())
            .collect::<Vec<_>>(),
        ["z", "m", "a"]
    );
    drop(values);
    assert_eq!(budget.used(), 0);
}
