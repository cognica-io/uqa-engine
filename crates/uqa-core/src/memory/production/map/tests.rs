//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    memory::{MemoryBudget, MemoryError},
    CancellationToken,
};

#[test]
fn ordered_production_scratch_tracks_exact_nodes_and_reuses_existing_keys() {
    let node_bytes = OwnedMap::<u32, u32>::entry_bytes();
    let budget = MemoryBudget::new(3 * node_bytes);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut map = ProductionMap::new(control);
    for (index, key) in [7, 2, 4].into_iter().enumerate() {
        assert_eq!(map.insert_copy(key, key * 10).unwrap(), None);
        assert_eq!(budget.used(), (index + 1) * node_bytes);
    }
    assert_eq!(map.insert_copy(2, 21).unwrap(), Some(20));
    assert_eq!(budget.used(), 3 * node_bytes);
    assert!(map.insert_copy(9, 90).is_err());
    assert_eq!(
        map.iter().map(|(k, v)| (*k, *v)).collect::<Vec<_>>(),
        [(2, 21), (4, 40), (7, 70)]
    );
    assert_eq!(map.remove(&4), Some(40));
    assert_eq!(budget.used(), 2 * node_bytes);
    map.insert_copy(9, 90).unwrap();
    assert_eq!(budget.used(), 3 * node_bytes);
    drop(map);
    assert_eq!(budget.used(), 0);
}

#[test]
fn ordered_production_scratch_rejects_before_allocation_and_preserves_cancelled_entries() {
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(0);
    let mut map = ProductionMap::new(ProductionControl::new(&budget, &token, &token));
    let allocation = allocation_counter::measure(|| {
        assert!(matches!(
            map.insert_copy(1_u32, 2_u32),
            Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
        ));
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    for original_scope in [true, false] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut map = ProductionMap::new(ProductionControl::new(&budget, &original, &invoking));
        map.insert_copy(1_u32, 2_u32).unwrap();
        let held = budget.used();
        if original_scope {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            map.insert_copy(1, 9),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(map.get(&1), Some(&2));
        assert_eq!(budget.used(), held);
        assert_eq!(map.remove(&1), Some(2));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn ordered_production_scratch_borrows_payloads_and_releases_nodes_on_unwind() {
    let key = "x".repeat(4 * 1024 * 1024);
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut map = ProductionMap::new(control);
        let allocation = allocation_counter::measure(|| {
            map.insert_copy(key.as_str(), 7_u32).unwrap();
        });
        assert_eq!(
            usize::try_from(allocation.bytes_current).unwrap(),
            budget.used()
        );
        assert_eq!(map.iter().next().unwrap().0.as_ptr(), key.as_ptr());
        panic!("consumer unwind");
    }));
    assert!(failure.is_err());
    assert_eq!(budget.used(), 0);
    let mut ordinary = ProductionMap::new(ProductionControl::uncontrolled());
    ordinary.insert_copy(key.as_str(), 7_u32).unwrap();
    assert_eq!(ordinary.get(key.as_str()), Some(&7));
    assert_eq!(ordinary.remove(key.as_str()), Some(7));
    assert!(ordinary.is_empty());
}
