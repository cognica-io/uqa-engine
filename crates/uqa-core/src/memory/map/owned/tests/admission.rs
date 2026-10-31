//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn mixed_allowances_retain_their_nodes_through_replacement_selection_and_move() {
    let first = MemoryBudget::new(4096);
    let second = MemoryBudget::new(4096);
    let bytes = OwnedMap::<i32, i32>::entry_bytes();
    let mut map = OwnedMap::new();
    map.insert_budgeted(1, 10, &first).unwrap();
    map.insert_budgeted(2, 20, &second).unwrap();
    map.insert_budgeted(3, 30, &first).unwrap();
    assert_eq!((first.used(), second.used()), (2 * bytes, bytes));
    let pointer = std::ptr::from_ref(map.get(&2).unwrap());
    assert_eq!(map.insert_budgeted(2, 21, &first).unwrap(), Some(20));
    let allocation = allocation_counter::measure(|| {
        map.retain(|key, value| {
            *value += 1;
            *key >= 2
        });
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!((first.used(), second.used()), (bytes, bytes));
    assert_eq!(std::ptr::from_ref(map.get(&2).unwrap()), pointer);
    assert_eq!(
        map.iter()
            .map(|(key, value)| (*key, *value))
            .collect::<Vec<_>>(),
        [(2, 22), (3, 31)]
    );
    let mut target = OwnedMap::new();
    target.append(map);
    assert_eq!((first.used(), second.used()), (bytes, bytes));
    target.clear();
    assert_eq!((first.used(), second.used()), (0, 0));
}

#[test]
fn failed_admission_does_not_publish_a_node_or_remove_existing_values() {
    let budget = MemoryBudget::new(OwnedMap::<u32, u32>::entry_bytes());
    let mut map = OwnedMap::new();
    map.insert_budgeted(1, 10, &budget).unwrap();
    assert!(map.insert_budgeted(2, 20, &budget).is_err());
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(&1), Some(&10));
    assert_eq!(map.insert_budgeted(1, 11, &budget).unwrap(), Some(10));
    drop(map);
    assert_eq!(budget.used(), 0);
}

#[test]
fn node_admission_precedes_value_production_and_failed_producers_preserve_entries() {
    let bytes = OwnedMap::<u32, u32>::entry_bytes();
    let budget = MemoryBudget::new(bytes);
    let mut map = OwnedMap::new();
    map.insert_budgeted_with(1, &budget, || -> Result<_, MemoryError> {
        assert_eq!(budget.used(), bytes);
        Ok(10)
    })
    .unwrap();
    let pointer = std::ptr::from_ref(map.get(&1).unwrap());
    let result = map.insert_budgeted_with(2, &budget, || -> Result<_, MemoryError> {
        panic!("a rejected node must not invoke its producer")
    });
    assert!(result.is_err());
    assert!(map
        .insert_budgeted_with(1, &budget, || -> Result<u32, MemoryError> {
            budget.reserve(1)?;
            Ok(20)
        })
        .is_err());
    assert_eq!(map.get(&1), Some(&10));
    assert_eq!(std::ptr::from_ref(map.get(&1).unwrap()), pointer);
    map.clear();
    assert!(map
        .insert_budgeted_with(2, &budget, || -> Result<u32, MemoryError> {
            budget.reserve(1)?;
            Ok(20)
        })
        .is_err());
    assert!(map.is_empty());
    assert_eq!(budget.used(), 0);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = map.insert_budgeted_with(3, &budget, || -> Result<u32, MemoryError> {
            assert_eq!(budget.used(), bytes);
            panic!("producer unwind")
        });
    }));
    assert!(unwind.is_err());
    assert_eq!(budget.used(), 0);
    assert!(map.is_empty());
}
