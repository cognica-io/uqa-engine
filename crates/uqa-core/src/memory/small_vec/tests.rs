//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn inline_elements_are_free_and_spills_charge_their_heap_buffer() {
    let budget = MemoryBudget::new(1024);
    let mut values = BudgetedSmallVec::<[u64; 4]>::new(&budget);
    for value in 0..4 {
        values.push(value).unwrap();
    }
    assert!(!values.spilled());
    assert_eq!(budget.used(), 0);
    values.push(4).unwrap();
    assert!(values.spilled());
    assert_eq!(budget.used(), values.capacity() * size_of::<u64>());
    for value in 5..20 {
        values.push(value).unwrap();
    }
    assert_eq!(&*values, (0..20).collect::<Vec<_>>().as_slice());
    assert_eq!(budget.used(), values.capacity() * size_of::<u64>());
    drop(values);
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_refused_spill_preserves_the_inline_elements_and_charges_nothing() {
    let budget = MemoryBudget::new(8);
    let mut values = BudgetedSmallVec::<[u64; 2]>::new(&budget);
    values.push(1).unwrap();
    values.push(2).unwrap();
    assert!(matches!(values.push(3), Err(MemoryError::Limit { .. })));
    assert!(!values.spilled());
    assert_eq!(&*values, [1, 2]);
    assert_eq!(budget.used(), 0);
}
