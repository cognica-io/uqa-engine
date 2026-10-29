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
