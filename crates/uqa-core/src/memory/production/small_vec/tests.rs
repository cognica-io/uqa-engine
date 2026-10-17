//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

#[test]
fn inline_production_scratch_admits_growth_and_releases_its_capacity() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut values = ProductionSmallVec::<[u32; 4]>::new(control);
    for value in 0..4 {
        values.push(value).unwrap();
    }
    assert_eq!(budget.used(), 0);
    values.push(4).unwrap();
    assert!(budget.used() >= 5 * size_of::<u32>());
    values.insert(2, 99).unwrap();
    assert_eq!(&*values, [0, 1, 99, 2, 3, 4]);
    assert_eq!(values.remove(2).unwrap(), 99);
    assert_eq!(&*values, [0, 1, 2, 3, 4]);
    drop(values);
    assert_eq!(budget.used(), 0);
}

#[test]
fn consumed_prefixes_are_not_shifted_and_are_compacted_before_growth() {
    fn check(control: ProductionControl<'_>) {
        let mut values = ProductionSmallVec::<[u32; 8]>::new(control);
        for value in 0..8 {
            values.push(value).unwrap();
        }
        let start = values.as_ptr();
        values.remove_prefix(6).unwrap();
        assert_eq!(values.as_ptr(), start.wrapping_add(6));
        assert_eq!(&*values, [6, 7]);
        values.insert(0, 5).unwrap();
        assert_eq!(values.as_ptr(), start.wrapping_add(5));
        values.push(8).unwrap();
        assert_eq!(values.as_ptr(), start);
        assert_eq!(&*values, [5, 6, 7, 8]);
        assert_eq!(values.remove(0).unwrap(), 5);
        assert_eq!(values.pop(), Some(8));
        assert_eq!(values.pop(), Some(7));
        assert_eq!(values.pop(), Some(6));
        assert_eq!(values.pop(), None);
        values.push(9).unwrap();
        assert_eq!(&*values, [9]);
    }
    check(ProductionControl::uncontrolled());
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    check(ProductionControl::new(&budget, &token, &token));
    assert_eq!(budget.used(), 0);
}

#[test]
fn scratch_refuses_growth_and_both_cancel_scopes_without_leaking() {
    let budget = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let mut values =
        ProductionSmallVec::<[u32; 2]>::new(ProductionControl::new(&budget, &original, &invoking));
    values.push(1).unwrap();
    values.push(2).unwrap();
    assert!(matches!(
        values.push(3),
        Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(&*values, [1, 2]);
    assert_eq!(budget.used(), 0);
    for original_scope in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut values = ProductionSmallVec::<[u32; 2]>::new(ProductionControl::new(
            &budget, &original, &invoking,
        ));
        values.push(1).unwrap();
        if original_scope {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(values.push(2).is_err());
        assert!(values.remove(0).is_err());
        assert!(values.insert(0, 2).is_err());
        assert!(values.remove_prefix(1).is_err());
        assert_eq!(&*values, [1]);
        assert_eq!(budget.used(), 0);
    }
}
