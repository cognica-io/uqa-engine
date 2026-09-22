//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use proptest::prelude::*;

use super::*;

fn verify<K: Ord, V>(link: &Link<K, V>) -> (usize, u8) {
    let Some(node) = link else { return (0, 0) };
    let (left, left_height) = verify(&node.left);
    let (right, right_height) = verify(&node.right);
    assert!(left_height.abs_diff(right_height) <= 1);
    assert_eq!(node.height, 1 + left_height.max(right_height));
    if let Some(left) = &node.left {
        assert!(left.entry.0 < node.entry.0);
    }
    if let Some(right) = &node.right {
        assert!(right.entry.0 > node.entry.0);
    }
    (left + right + 1, node.height)
}

proptest! {
    #[test]
    fn retained_roots_match_independent_ordered_maps(
        operations in prop::collection::vec((any::<bool>(), 0_u8..80, any::<i32>()), 1..160)
    ) {
        let budget = MemoryBudget::new(1 << 20);
        let mut map = BudgetedSharedMap::new(&budget);
        let mut expected = BTreeMap::new();
        let mut retained = Vec::new();
        for (capture, key, value) in operations {
            if capture {
                let used = budget.used();
                retained.push((map.clone(), expected.clone()));
                prop_assert_eq!(budget.used(), used);
            }
            map = map.with_insert(key, value).unwrap();
            expected.insert(key, value);
            prop_assert_eq!(map.len(), expected.len());
            prop_assert_eq!(map.is_empty(), expected.is_empty());
            prop_assert_eq!(verify(&map.root).0, map.len());
            prop_assert!(map.iter().eq(expected.iter()));
        }
        drop(map);
        for (snapshot, expected) in retained {
            prop_assert!(snapshot.iter().eq(expected.iter()));
            prop_assert_eq!(verify(&snapshot.root).0, expected.len());
            for key in 0..80 {
                prop_assert_eq!(snapshot.get(&key), expected.get(&key));
            }
        }
        prop_assert_eq!(budget.used(), 0);
    }
}

#[test]
fn sorted_updates_balance_and_reclaim_every_unreachable_node() {
    for reverse in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let mut map = BudgetedSharedMap::<usize, usize>::new(&budget);
        let node_bytes = size_of::<Budgeted<Node<usize, usize>>>();
        let entry_bytes = size_of::<Budgeted<(usize, usize)>>();
        for next in 0..2048 {
            let key = if reverse { 2047 - next } else { next };
            map = map.with_insert(key, key).unwrap();
            assert_eq!(budget.used(), map.len() * (node_bytes + entry_bytes));
        }
        assert_eq!(verify(&map.root).0, 2048);
        for key in 0..2048 {
            map = map.with_insert(key, key * 2).unwrap();
            assert_eq!(budget.used(), map.len() * (node_bytes + entry_bytes));
        }
        assert!(map.iter().all(|(key, value)| *value == key * 2));
        drop(map);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn borrowed_ranges_seek_inclusively_and_exclusively_without_allocating() {
    let budget = MemoryBudget::new(1 << 16);
    let mut map = BudgetedSharedMap::new(&budget);
    let expected = BTreeMap::from_iter([("a", 1), ("c", 3), ("f", 6), ("m", 13)]);
    for (key, value) in &expected {
        map = map.with_insert((*key).to_owned(), *value).unwrap();
    }
    let used = budget.used();
    for key in ["", "a", "b", "c", "m", "z"] {
        assert_eq!(map.get(key), expected.get(key));
        for bound in [Bound::Included(key), Bound::Excluded(key)] {
            assert!(map
                .range_from(bound)
                .map(|(key, value)| (key.as_str(), value))
                .eq(expected
                    .range::<str, _>((bound, Bound::Unbounded))
                    .map(|(key, value)| (*key, value))));
        }
    }
    let mut iter = map.range_from::<str>(Bound::Unbounded);
    assert_eq!(iter.by_ref().count(), 4);
    assert!(iter.next().is_none());
    assert!(iter.next().is_none());
    assert_eq!(budget.used(), used);
    let empty = BudgetedSharedMap::<String, usize>::new(&budget);
    assert!(empty.range_from(Bound::Included("a")).next().is_none());
}

#[test]
fn nonclone_entries_keep_addresses_and_drop_with_the_last_referencing_root() {
    struct Value(Arc<AtomicUsize>);
    impl Drop for Value {
        fn drop(&mut self) {
            self.0.fetch_add(1, AtomicOrdering::Relaxed);
        }
    }
    let budget = MemoryBudget::new(1 << 16);
    let drops = Arc::new(AtomicUsize::new(0));
    let original = BudgetedSharedMap::new(&budget)
        .with_insert(1, Value(drops.clone()))
        .unwrap()
        .with_insert(2, Value(drops.clone()))
        .unwrap();
    let first = std::ptr::from_ref(original.get(&1).unwrap());
    let retained = original.clone();
    let changed = original.with_insert(2, Value(drops.clone())).unwrap();
    assert_eq!(std::ptr::from_ref(changed.get(&1).unwrap()), first);
    drop(original);
    assert_eq!(drops.load(AtomicOrdering::Relaxed), 0);
    drop(retained);
    assert_eq!(drops.load(AtomicOrdering::Relaxed), 1);
    drop(changed);
    assert_eq!(drops.load(AtomicOrdering::Relaxed), 3);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_path_and_rotation_reservations_preserve_roots_and_release_candidates() {
    let mut failures = 0;
    let mut successes = 0;
    for (initial, next) in [
        ([30, 20], 10),
        ([30, 10], 20),
        ([10, 30], 20),
        ([10, 20], 30),
    ] {
        for allowance in (0..2048).step_by(16) {
            let budget = MemoryBudget::new(1 << 16);
            let mut map = BudgetedSharedMap::new(&budget);
            for key in initial {
                map = map.with_insert(key, key).unwrap();
            }
            let before = budget.used();
            let held = budget.reserve(budget.limit() - before - allowance).unwrap();
            match map.with_insert(next, next) {
                Ok(candidate) => {
                    successes += 1;
                    assert_eq!(verify(&candidate.root).0, 3);
                    assert_eq!(candidate.get(&next), Some(&next));
                }
                Err(MemoryError::Limit { .. }) => failures += 1,
                Err(error) => panic!("unexpected insertion error: {error}"),
            }
            assert_eq!(map.len(), 2);
            assert!(map.get(&next).is_none());
            drop(held);
            assert_eq!(budget.used(), before);
            drop(map);
            assert_eq!(budget.used(), 0);
        }
    }
    assert!(failures > 1 && successes > 1);
}
