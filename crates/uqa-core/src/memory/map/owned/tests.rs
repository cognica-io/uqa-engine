//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::{BudgetedMap, MemoryBudget};
use proptest::prelude::*;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

mod admission;
mod serialization;

#[test]
fn extracting_and_restoring_preserves_node_identity_and_admission_without_allocation() {
    let budget = MemoryBudget::new(1 << 20);
    let mut map = OwnedMap::new();
    for key in 0..31 {
        map.insert_budgeted(key, key * 2, &budget).unwrap();
    }
    let used = budget.used();
    for key in [15, 0, 30, 7, 22] {
        let pointer = std::ptr::from_ref(map.get(&key).unwrap());
        let measured = allocation_counter::measure(|| {
            let entry = map.extract(&key);
            assert_eq!(budget.used(), used);
            assert_eq!(entry.len(), 1);
            assert_eq!(std::ptr::from_ref(entry.get(&key).unwrap()), pointer);
            assert_eq!(super::super::tests::verify(&map.root).0, map.len());
            map.append(entry);
            assert_eq!(budget.used(), used);
            assert_eq!(std::ptr::from_ref(map.get(&key).unwrap()), pointer);
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(super::super::tests::verify(&map.root).0, 31);
    }
    assert!(map.extract(&99).is_empty());
    let entry = map.extract(&15);
    drop(map);
    assert_eq!(budget.used(), OwnedMap::<i32, i32>::entry_bytes());
    drop(entry);
    assert_eq!(budget.used(), 0);
}

proptest! {
    #[test]
    fn appending_preserves_nodes_and_matches_ordered_map_collision_semantics(
        left in prop::collection::vec((0_u8..100, any::<i32>()), 0..200),
        right in prop::collection::vec((0_u8..100, any::<i32>()), 0..200),
    ) {
        let mut expected: BTreeMap<_, _> = left.iter().copied().collect();
        expected.extend(right.iter().copied());
        let mut target: OwnedMap<_, _> = left.into_iter().collect();
        let source: OwnedMap<_, _> = right.into_iter().collect();
        let mut addresses: BTreeMap<_, _> = source.iter()
            .map(|(key, value)| (*key, std::ptr::from_ref(value))).collect();
        addresses.extend(target.iter().map(|(key, value)| (*key, std::ptr::from_ref(value))));
        target.append(source);
        prop_assert!(target.iter().eq(expected.iter()));
        prop_assert_eq!(super::super::tests::verify(&target.root).0, target.len());
        for (key, value) in &target {
            prop_assert_eq!(std::ptr::from_ref(value), addresses[key]);
        }
    }

    #[test]
    fn ordinary_mutations_share_balancing_and_exact_layout_with_admitted_maps(
        operations in prop::collection::vec((0_u8..3, 0_u8..100, any::<i32>()), 0..300)
    ) {
        let mut ordinary = OwnedMap::new();
        let budget = MemoryBudget::new(1 << 20);
        let mut admitted = BudgetedMap::new(&budget);
        let mut expected = BTreeMap::new();
        for (operation, key, value) in operations {
            match operation {
                0 => {
                    let previous = expected.insert(key, value);
                    assert_eq!(ordinary.insert(key, value), previous);
                    assert_eq!(admitted.insert(key, value).unwrap(), previous);
                }
                1 => {
                    let previous = expected.remove(&key);
                    assert_eq!(ordinary.remove(&key), previous);
                    assert_eq!(admitted.remove(&key), previous);
                }
                _ => assert_eq!(ordinary.get(&key), expected.get(&key)),
            }
            assert!(ordinary.iter().eq(expected.iter()));
            assert_eq!(ordinary.allocated_bytes(), budget.used());
            assert_eq!(super::super::tests::verify(&ordinary.root).0, ordinary.len());
        }
        assert!(ordinary.into_iter().eq(expected));
        drop(admitted);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn appending_releases_replaced_values_once_and_preserves_the_original_key() {
    struct Value(Arc<AtomicUsize>);
    impl Drop for Value {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let mut key = String::with_capacity(128);
    key.push_str("alpha");
    let pointer = key.as_ptr();
    let mut target: OwnedMap<_, _> = [(key, Value(Arc::clone(&drops)))].into_iter().collect();
    let source = ["alpha", "omega"]
        .into_iter()
        .map(|key| (key.to_owned(), Value(Arc::clone(&drops))))
        .collect();
    target.append(source);
    assert_eq!(target.keys().next().unwrap().as_ptr(), pointer);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(target.len(), 2);
    drop(target);
    assert_eq!(drops.load(Ordering::Relaxed), 3);
}

#[test]
fn replacement_keeps_the_original_key_and_borrowed_bounds_seek_exactly() {
    let mut map = OwnedMap::new();
    let mut alpha = String::with_capacity(128);
    alpha.push_str("alpha");
    let pointer = alpha.as_ptr();
    map.insert(alpha, 1);
    map.insert("omega".to_owned(), 9);
    assert_eq!(map.insert("alpha".to_owned(), 2), Some(1));
    assert_eq!(map.keys().next().unwrap().as_ptr(), pointer);
    assert_eq!(map.first_from::<str>(Bound::Unbounded).unwrap().1, &2);
    assert_eq!(map.first_from(Bound::Included("alpha")).unwrap().1, &2);
    assert_eq!(map.first_from(Bound::Excluded("alpha")).unwrap().1, &9);
    assert_eq!(map.first_from(Bound::Included("middle")).unwrap().1, &9);
    assert!(map.first_from(Bound::Excluded("omega")).is_none());
    assert_eq!(map.get("alpha"), Some(&2));
    *map.get_mut("alpha").unwrap() = 3;
    let cloned = map.clone();
    assert_ne!(cloned.keys().next().unwrap().as_ptr(), pointer);
    let (key, value) = map.remove_entry("alpha").unwrap();
    assert_eq!(key.as_ptr(), pointer);
    assert_eq!(value, 3);
    assert_eq!(cloned.get("alpha"), Some(&3));
}

#[test]
fn set_collisions_and_full_document_id_bounds_keep_order() {
    let set: OwnedSet<_> = [u64::MAX, 0, 9, 9].into_iter().collect();
    assert_eq!(set.len(), 3);
    assert!(set.iter().copied().eq([0, 9, u64::MAX]));
    assert_eq!(set.allocated_bytes(), 3 * OwnedSet::<u64>::entry_bytes());
    assert!(set.contains(&9));
    assert!(set.into_iter().eq([0, 9, u64::MAX]));
    let map: OwnedMap<_, _> = [(u64::MAX, 1), (0, 2)].into_iter().collect();
    assert_eq!(map.first_from(Bound::Excluded(&0)), Some((&u64::MAX, &1)));
    assert!(map.first_from(Bound::Excluded(&u64::MAX)).is_none());
}

#[test]
fn dropping_a_partial_owned_traversal_frees_each_remaining_value_once() {
    struct Value(Arc<AtomicUsize>);
    impl Drop for Value {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let map: OwnedMap<_, _> = (0..512)
        .map(|key| (key, Value(Arc::clone(&drops))))
        .collect();
    let mut iter = map.into_iter();
    assert_eq!(iter.len(), 512);
    let first = iter.next().unwrap();
    assert_eq!(first.0, 0);
    assert_eq!(iter.len(), 511);
    drop(iter);
    assert_eq!(drops.load(Ordering::Relaxed), 511);
    drop(first);
    assert_eq!(drops.load(Ordering::Relaxed), 512);
}

#[test]
fn owned_sets_preserve_ordered_sequence_storage_and_exact_node_ownership() {
    let reference = std::collections::BTreeSet::from(["a", "b", "z"]);
    let mut set: OwnedSet<String> = serde_json::from_str(r#"["z","a","b","a"]"#).unwrap();
    assert_eq!(
        serde_json::to_string(&set).unwrap(),
        serde_json::to_string(&reference).unwrap()
    );
    assert_eq!(set.allocated_bytes(), 3 * OwnedSet::<String>::entry_bytes());
    assert!(set.remove("b"));
    assert!(!set.remove("b"));
    assert_eq!(set.allocated_bytes(), 2 * OwnedSet::<String>::entry_bytes());
    set.extend(["c".to_owned(), "a".to_owned()]);
    assert_eq!(
        set.iter().map(String::as_str).collect::<Vec<_>>(),
        ["a", "c", "z"]
    );
    assert_eq!(set.allocated_bytes(), 3 * OwnedSet::<String>::entry_bytes());
    let copied = set.clone();
    set.clear();
    assert!(set.is_empty());
    assert_eq!(set.allocated_bytes(), 0);
    assert_eq!(copied.len(), 3);
    assert!(serde_json::from_str::<OwnedSet<String>>(r#"["a",1]"#).is_err());
}

#[test]
fn mutable_values_keep_disjoint_borrows_without_allocating_or_releasing_nodes() {
    let budget = crate::memory::MemoryBudget::new(1 << 20);
    let mut map = OwnedMap::new();
    for key in (0..1024).rev() {
        map.insert_budgeted(key, key, &budget).unwrap();
    }
    let retained = budget.used();
    let measured = allocation_counter::measure(|| {
        let mut values = map.values_mut();
        assert_eq!(values.len(), 1024);
        let first = values.next().unwrap();
        for (index, value) in values.enumerate() {
            assert_eq!(*value, index + 1);
            *value += 100;
        }
        *first = 9000;
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(budget.used(), retained);
    assert_eq!(map.get(&0), Some(&9000));
    assert_eq!(map.get(&1023), Some(&1123));
    drop(map);
    assert_eq!(budget.used(), 0);
}

#[test]
fn produced_text_keys_keep_payload_leases_through_replacement_and_removal() {
    use crate::memory::{Produced, ProductionControl};
    let budget = crate::memory::MemoryBudget::new(1 << 20);
    let token = crate::CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut map: OwnedMap<Produced<String>, usize> = OwnedMap::new();
    map.insert_budgeted(control.copy_text("saved").unwrap(), 1, &budget)
        .unwrap();
    let retained = budget.used();
    map.insert_budgeted(control.copy_text("saved").unwrap(), 2, &budget)
        .unwrap();
    assert_eq!(budget.used(), retained);
    assert_eq!(map.get("saved"), Some(&2));
    assert_eq!(map.remove("saved"), Some(2));
    assert_eq!(budget.used(), 0);
}

#[test]
fn set_transfer_preserves_nodes_and_releases_duplicate_owners() {
    use crate::memory::{OwnedSet, Produced, ProductionControl};
    let budget = crate::memory::MemoryBudget::new(1 << 20);
    let token = crate::CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = budget.reserve(37).unwrap();
    let mut left = OwnedSet::new();
    let mut right = OwnedSet::new();
    for (set, name) in [(&mut left, "first"), (&mut right, "first")] {
        set.insert_budgeted(control.copy_text(name).unwrap(), &budget)
            .unwrap();
    }
    right
        .insert_budgeted(control.copy_text("second").unwrap(), &budget)
        .unwrap();
    let measured = allocation_counter::measure(|| {
        left.append(right);
        assert!(left.contains_by(|name| "first".cmp(name.as_str())));
        assert!(!left.contains_by(|name| "missing".cmp(name.as_str())));
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(left.len(), 2);
    assert_eq!(
        budget.used(),
        37 + 2 * OwnedSet::<Produced<String>>::entry_bytes()
            + left.iter().map(|name| name.capacity()).sum::<usize>()
    );
    drop(left);
    assert_eq!(budget.used(), 37);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
