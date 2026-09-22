//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use proptest::prelude::*;

use super::*;

fn verify<K: Ord, V>(link: &Link<K, V>) -> (usize, u8) {
    let Some(node) = link else { return (0, 0) };
    let (left, left_height) = verify(&node.left);
    let (right, right_height) = verify(&node.right);
    assert!(left_height.abs_diff(right_height) <= 1);
    assert_eq!(node.height, 1 + left_height.max(right_height));
    if let Some(left) = &node.left {
        assert!(left.key < node.key);
    }
    if let Some(right) = &node.right {
        assert!(right.key > node.key);
    }
    (left + right + 1, node.height)
}

proptest! {
    #[test]
    fn mixed_mutations_match_ordered_map_and_charge_every_retained_node(
        operations in prop::collection::vec((0_u8..3, 0_u8..100, any::<i32>()), 0..300)
    ) {
        let budget = MemoryBudget::new(1 << 20);
        let mut map = BudgetedMap::new(&budget);
        let mut expected = BTreeMap::new();
        for (operation, key, value) in operations {
            match operation {
                0 => assert_eq!(map.insert(key, value).unwrap(), expected.insert(key, value)),
                1 => assert_eq!(map.remove(&key), expected.remove(&key)),
                _ => assert_eq!(map.get(&key), expected.get(&key)),
            }
            assert_eq!(map.len(), expected.len());
            assert_eq!(map.is_empty(), expected.is_empty());
            assert!(map.iter().eq(expected.iter()));
            assert_eq!(verify(&map.root).0, map.len());
            assert_eq!(budget.used(), map.len() * size_of::<Node<u8, i32>>());
            let mut iter = map.iter();
            for remaining in (1..=map.len()).rev() {
                assert_eq!(iter.len(), remaining);
                assert!(iter.next().is_some());
            }
            assert_eq!(iter.len(), 0);
            assert!(iter.next().is_none());
            assert!(iter.next().is_none());
        }
        drop(map);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn ordered_inputs_and_removals_preserve_balance_and_allocation_free_updates() {
    for reverse in [false, true] {
        let budget = MemoryBudget::new(4096 * size_of::<Node<usize, usize>>());
        let mut map = BudgetedMap::<usize, usize>::new(&budget);
        for next in 0..4096 {
            let key = if reverse { 4095 - next } else { next };
            map.insert(key, key).unwrap();
        }
        assert_eq!(verify(&map.root).0, 4096);
        assert_eq!(budget.used(), budget.limit());
        let before = budget.peak();
        map.for_each_mut(|key, value| *value += key);
        assert!(map.iter().all(|(key, value)| *value == key * 2));
        for key in (0..4096).step_by(2) {
            assert_eq!(map.remove(&key), Some(key * 2));
        }
        assert_eq!(verify(&map.root).0, 2048);
        for key in (1..4096).step_by(2).rev() {
            assert_eq!(map.remove(&key), Some(key * 2));
        }
        assert!(map.is_empty());
        assert_eq!(budget.used(), 0);
        assert_eq!(budget.peak(), before);
    }
}

#[test]
fn unpublished_entries_charge_full_nodes_and_failed_preparation_preserves_the_map() {
    let bytes = size_of::<Node<u64, u64>>();
    let budget = MemoryBudget::new(3 * bytes);
    let mut map = BudgetedMap::<u64, u64>::new(&budget);
    map.insert(4, 40).unwrap();
    let first = map.prepare_entry(2, 20).unwrap();
    let second = map.prepare_entry(6, 60).unwrap();
    assert_eq!(budget.used(), budget.limit());
    assert!(matches!(
        map.prepare_entry(8, 80),
        Err(MemoryError::Limit { .. })
    ));
    assert!(matches!(map.insert(8, 80), Err(MemoryError::Limit { .. })));
    assert_eq!(map.len(), 1);
    assert_eq!(map[&4], 40);
    assert_eq!(map.insert(4, 41).unwrap(), Some(40));
    assert_eq!(map.insert_prepared(first), None);
    assert_eq!(map.insert_prepared(second), None);
    assert_eq!(budget.used(), 3 * bytes);
    assert_eq!(budget.peak(), 3 * bytes);
    assert!(map
        .iter()
        .eq([(2, 20), (4, 41), (6, 60)].iter().map(|(k, v)| (k, v))));
    assert_eq!(map.remove(&2), Some(20));
    let discarded = map.prepare_entry(9, 90).unwrap();
    drop(discarded);
    assert_eq!(budget.used(), 2 * bytes);
    drop(map);
    assert_eq!(budget.used(), 0);
}

#[test]
fn borrowed_keys_and_prepared_replacements_preserve_original_key_ownership() {
    let budget = MemoryBudget::new(1 << 16);
    let mut map = BudgetedMap::new(&budget);
    map.insert("alpha".to_owned(), "old".to_owned()).unwrap();
    let pointer = map.iter().next().unwrap().0.as_ptr();
    let replacement = map
        .prepare_entry("alpha".to_owned(), "new".to_owned())
        .unwrap();
    assert_eq!(map.insert_prepared(replacement).as_deref(), Some("old"));
    assert_eq!(map.iter().next().unwrap().0.as_ptr(), pointer);
    assert_eq!(map.get("alpha").map(String::as_str), Some("new"));
    map.get_mut("alpha").unwrap().push('!');
    assert_eq!(map.remove("absent"), None);
    assert_eq!(map.remove("alpha").as_deref(), Some("new!"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn prepared_entries_cannot_move_reservations_between_allowances() {
    let original = MemoryBudget::new(4096);
    let foreign = MemoryBudget::new(4096);
    let mut map = BudgetedMap::new(&original);
    map.insert(1_u32, 10_u32).unwrap();
    let other = BudgetedMap::new(&foreign);
    let entry = other.prepare_entry(2, 20).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        map.insert_prepared(entry);
    }))
    .is_err());
    assert_eq!(map.len(), 1);
    assert_eq!(map[&1], 10);
    assert_eq!(foreign.used(), 0);
    assert_eq!(original.used(), size_of::<Node<u32, u32>>());
}
