//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_map_mutation_allowance_transfer_keeps_original_nodes_and_payload_owners() {
    let input = MemoryBudget::new(4096);
    let runtime = MemoryBudget::new(4096);
    let capture = MemoryBudget::new(4096);
    let mut payload = crate::memory::BudgetedString::new(&input);
    payload.push_str("original").unwrap();
    let pointer = payload.as_ptr();
    let mut map = RetainedMap::new(&input);
    map.insert(1, payload, 8).unwrap();
    let original = map.capture(&capture).unwrap();
    let occupied = input.reserve(input.available()).unwrap();
    let used = input.used();
    let mut transferred = None;
    let allocations = allocation_counter::measure(|| {
        transferred = Some(map.into_mutation_budget(&runtime));
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(input.used(), used);
    let mut map = transferred.unwrap();
    assert_eq!(map.get(&1).unwrap().as_ptr(), pointer);
    let mut next = crate::memory::BudgetedString::new(&runtime);
    next.push_str("later").unwrap();
    map.insert(2, next, 5).unwrap();
    assert!(map.remove(&1).unwrap());
    assert_eq!(original.get(&1).unwrap().as_ptr(), pointer);
    assert_eq!(&**original.get(&1).unwrap(), "original");
    drop(map);
    assert_eq!(runtime.used(), 0);
    assert!(input.used() > occupied.bytes());
    drop(original);
    assert_eq!(capture.used(), 0);
    assert_eq!(input.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(input.used(), 0);
}

#[test]
fn retained_map_lower_bound_seeks_without_allocating_or_visiting_earlier_keys() {
    use std::{
        cmp::Ordering,
        ops::Bound,
        sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
    };
    static COMPARES: AtomicUsize = AtomicUsize::new(0);
    #[derive(Eq, PartialEq)]
    struct Key(u32);
    impl Ord for Key {
        fn cmp(&self, other: &Self) -> Ordering {
            COMPARES.fetch_add(1, AtomicOrdering::Relaxed);
            self.0.cmp(&other.0)
        }
    }
    impl PartialOrd for Key {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }
    let mut map = RetainedMap::new(&MemoryBudget::new(1 << 20));
    for key in 0..4096 {
        map.insert(Key(key), key, 0).unwrap();
    }
    for excluded in [false, true] {
        COMPARES.store(0, AtomicOrdering::Relaxed);
        let allocation = allocation_counter::measure(|| {
            let key = Key(4000);
            let bound = if excluded {
                Bound::Excluded(&key)
            } else {
                Bound::Included(&key)
            };
            assert!(map
                .range_from(bound)
                .map(|(key, value)| (key.0, *value))
                .eq((4000 + u32::from(excluded)..4096).map(|key| (key, key))));
        });
        assert_eq!(allocation.count_total, 0);
        assert!(COMPARES.load(AtomicOrdering::Relaxed) < 32);
    }
    let captured = MemoryBudget::new(1 << 20);
    let snapshot = map.capture(&captured).unwrap();
    map.insert(Key(4000), u32::MAX, 0).unwrap();
    map.remove(&Key(4001)).unwrap();
    drop(map);
    for (bound, expected) in [
        (Bound::Unbounded, Some(0)),
        (Bound::Included(&Key(4000)), Some(4000)),
        (Bound::Excluded(&Key(4000)), Some(4001)),
        (Bound::Included(&Key(4095)), Some(4095)),
        (Bound::Excluded(&Key(4095)), None),
        (Bound::Included(&Key(u32::MAX)), None),
    ] {
        COMPARES.store(0, AtomicOrdering::Relaxed);
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                snapshot
                    .first_from(bound)
                    .map(|(key, value)| (key.0, *value)),
                expected.map(|key| (key, key)),
            );
        });
        assert_eq!(allocation.count_total, 0);
        assert!(COMPARES.load(AtomicOrdering::Relaxed) < 32);
    }
    drop(snapshot);
    assert_eq!(captured.used(), 0);
}

#[test]
fn retained_map_captures_nonclone_payloads_without_allocating_or_visiting_entries() {
    struct Payload(usize);
    for entries in [1, 64, 4096] {
        let construction = MemoryBudget::new(8 << 20);
        let mut source = RetainedMap::new(&construction);
        for key in 0..entries {
            source.insert(key, Payload(key * 2), 1024).unwrap();
        }
        let first = MemoryBudget::new(8 << 20);
        let second = MemoryBudget::new(8 << 20);
        let mut saved = None;
        let usage = allocation_counter::measure(|| saved = Some(source.capture(&first).unwrap()));
        assert_eq!(usage.count_total, 0);
        let saved = saved.unwrap();
        let independent = source.capture(&second).unwrap();
        assert_eq!(first.used(), second.used());
        assert!(first.used() >= entries * 1024);
        let original = source.clone();
        source.insert(0, Payload(99), 8192).unwrap();
        source.clear();
        drop((source, original));
        assert_eq!(saved.get(&0).unwrap().0, 0);
        assert!(saved
            .iter()
            .map(|(key, value)| (*key, value.0))
            .eq((0..entries).map(|key| (key, key * 2))));
        drop(saved);
        assert_eq!(first.used(), 0);
        assert!(second.used() > 0);
        drop(independent);
        assert_eq!(second.used(), 0);
        assert_eq!(construction.used(), 0);
    }
}

#[test]
fn retained_map_failed_updates_and_overflow_preserve_root_and_footprint() {
    let construction = MemoryBudget::new(8192);
    let capture = MemoryBudget::new(8192);
    let mut source = RetainedMap::new(&construction);
    source.insert("a".to_owned(), 1, 100).unwrap();
    source.insert("b".to_owned(), 2, 200).unwrap();
    let saved = source.capture(&capture).unwrap();
    let retained = capture.used();
    let occupied = construction.reserve(construction.available()).unwrap();
    assert!(source.insert("a".into(), 3, 300).is_err());
    assert!(source.insert("c".into(), 3, 300).is_err());
    assert!(source.remove("b").is_err());
    assert!(!source.remove("missing").unwrap());
    assert!(matches!(
        source.insert("c".into(), 4, usize::MAX),
        Err(MemoryError::SizeOverflow)
    ));
    drop(occupied);
    let after = source.capture(&capture).unwrap();
    assert_eq!(capture.used(), retained * 2);
    assert!(saved.iter().eq(after.iter()));
    source.insert("a".into(), 9, 500).unwrap();
    let replaced = source.capture(&capture).unwrap();
    assert_eq!(replaced.reserved_bytes(), retained + 400);
    assert!(source.remove("b").unwrap());
    assert_eq!(source.get("a"), Some(&9));
    assert!(source.get("b").is_none());
    drop((source, saved, after, replaced));
    assert_eq!(capture.used(), 0);
    assert_eq!(construction.used(), 0);
}

#[test]
fn retained_map_capture_admits_exact_root_and_releases_failed_admission() {
    let mut source = RetainedMap::new(&MemoryBudget::new(4096));
    source.insert("one", (), 512).unwrap();
    let first = source.capture(&MemoryBudget::new(4096)).unwrap();
    let bytes = first.reserved_bytes();
    assert_eq!(source.retained_bytes().unwrap(), bytes);
    let denied = MemoryBudget::new(bytes - 1);
    assert!(source.capture(&denied).is_err());
    assert_eq!(denied.used(), 0);
    let exact = MemoryBudget::new(bytes);
    let second = source.capture(&exact).unwrap();
    assert_eq!(exact.used(), bytes);
    source.insert("one", (), usize::MAX).unwrap();
    assert!(matches!(
        source.capture(&MemoryBudget::new(usize::MAX)),
        Err(MemoryError::SizeOverflow)
    ));
    assert!(matches!(
        source.retained_bytes(),
        Err(MemoryError::SizeOverflow)
    ));
    assert_eq!(first.len(), 1);
    source.clear();
    assert!(source.capture(&MemoryBudget::new(0)).unwrap().is_empty());
    drop(second);
    assert_eq!(exact.used(), 0);
}

#[test]
fn retained_map_both_ends_share_original_values_and_exact_footprint() {
    let memory = MemoryBudget::new(1 << 20);
    let capture = MemoryBudget::new(1 << 20);
    let mut source = RetainedMap::new(&memory);
    for key in 0..128 {
        source.insert(key, key * 2, key).unwrap();
    }
    let mut saved = None;
    let allocations = allocation_counter::measure(|| {
        let footprint = source.retained_bytes().unwrap();
        saved = Some(source.capture(&capture).unwrap());
        assert_eq!(capture.used(), footprint);
        assert!(source
            .iter_bidirectional()
            .rev()
            .map(|(k, v)| (*k, *v))
            .eq((0..128).rev().map(|k| (k, k * 2))));
    });
    assert_eq!(allocations.count_total, 0);
    source.insert(0, 9, 9999).unwrap();
    drop(source);
    let saved = saved.unwrap();
    let mut cursor = saved.iter_bidirectional();
    assert_eq!(cursor.next(), Some((&0, &0)));
    assert_eq!(cursor.next_back(), Some((&127, &254)));
    assert_eq!(cursor.len(), 126);
    assert!(cursor
        .map(|(k, v)| (*k, *v))
        .eq((1..127).map(|k| (k, k * 2))));
    drop(saved);
    assert_eq!(capture.used(), 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn prepared_retained_publication_preserves_cached_footprints_and_failed_roots() {
    let memory = MemoryBudget::new(1 << 20);
    let capture = MemoryBudget::new(1 << 20);
    let mut source = RetainedMap::new(&memory);
    source.insert(1, 10, 100).unwrap();
    source.insert(2, 20, 200).unwrap();
    let old = source.capture(&capture).unwrap();
    let footprint = source.retained_bytes().unwrap();
    let used = memory.used();
    drop(source.prepare_insert(1, 500).unwrap());
    assert_eq!(source.retained_bytes().unwrap(), footprint);
    assert_eq!(memory.used(), used);
    assert!(matches!(
        source.prepare_insert(3, usize::MAX),
        Err(MemoryError::SizeOverflow)
    ));
    let occupied = memory.reserve(memory.available()).unwrap();
    assert!(source.prepare_insert(1, 500).is_err());
    drop(occupied);
    assert_eq!(source.retained_bytes().unwrap(), footprint);
    assert_eq!(memory.used(), used);
    let prepared = source.prepare_insert(1, 500).unwrap();
    let occupied = memory.reserve(memory.available()).unwrap();
    prepared.publish(99);
    drop(occupied);
    assert_eq!(source.retained_bytes().unwrap(), footprint + 400);
    assert_eq!(source.get(&1), Some(&99));
    assert_eq!(old.get(&1), Some(&10));
    let current = source.capture(&capture).unwrap();
    assert_eq!(current.reserved_bytes(), footprint + 400);
    drop((source, old, current));
    assert_eq!(capture.used(), 0);
    assert_eq!(memory.used(), 0);
}
