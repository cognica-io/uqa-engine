//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

proptest! {
    #[test]
    fn immutable_deletions_and_insertions_preserve_every_retained_root(
        operations in prop::collection::vec((any::<bool>(), any::<bool>(), 0_u8..80, any::<i32>()), 1..200)
    ) {
        let budget = MemoryBudget::new(1 << 20);
        let mut map = BudgetedSharedMap::new(&budget);
        let mut expected = BTreeMap::new();
        let mut retained = Vec::new();
        for (capture, remove, key, value) in operations {
            if capture {
                retained.push((map.snapshot(), expected.clone()));
            }
            if remove {
                map = map.with_remove(&key).unwrap();
                expected.remove(&key);
            } else {
                map.try_insert(key, value).unwrap();
                expected.insert(key, value);
            }
            prop_assert_eq!(map.len(), expected.len());
            prop_assert_eq!(verify(&map.root).0, expected.len());
            prop_assert!(map.iter().eq(expected.iter()));
        }
        drop(map);
        for (snapshot, expected) in retained {
            prop_assert_eq!(snapshot.len(), expected.len());
            prop_assert_eq!(verify(&snapshot.root).0, expected.len());
            prop_assert!(snapshot.iter().eq(expected.iter()));
        }
        prop_assert_eq!(budget.used(), 0);
    }
}

#[test]
fn deletion_rotations_preserve_order_and_retire_unreachable_nodes() {
    for reverse_insert in [false, true] {
        for reverse_remove in [false, true] {
            let budget = MemoryBudget::new(1 << 20);
            let mut map = BudgetedSharedMap::new(&budget);
            for i in 0..128 {
                let key = if reverse_insert { 127 - i } else { i };
                map.try_insert(key, key * 3).unwrap();
            }
            let mut expected = (0..128).map(|i| (i, i * 3)).collect::<BTreeMap<_, _>>();
            for i in 0..128 {
                let key = if reverse_remove { 127 - i } else { i };
                let before = map.snapshot();
                map = map.with_remove(&key).unwrap();
                assert!(before.iter().eq(expected.iter()));
                expected.remove(&key);
                assert!(map.iter().eq(expected.iter()));
                assert_eq!(verify(&map.root).0, expected.len());
                drop(before);
                assert_eq!(budget.used(), map.snapshot().structural_bytes().unwrap());
            }
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn deletion_admission_failure_and_absent_keys_preserve_the_original_root() {
    let budget = MemoryBudget::new(1 << 16);
    let mut map = BudgetedSharedMap::new(&budget);
    for key in 0..64 {
        map.try_insert(key, key).unwrap();
    }
    let baseline = budget.used();
    let original = map.snapshot();
    let held = budget.reserve(budget.available()).unwrap();
    assert!(matches!(
        map.with_remove(&31),
        Err(MemoryError::Limit { .. })
    ));
    let allocation = allocation_counter::measure(|| {
        let unchanged = map.with_remove(&100).unwrap();
        assert!(Arc::ptr_eq(
            unchanged.root.as_ref().unwrap(),
            map.root.as_ref().unwrap()
        ));
    });
    assert_eq!(allocation.bytes_total, 0);
    assert!(map.iter().eq(original.iter()));
    drop(held);
    assert_eq!(budget.used(), baseline);
    for available in (0..4096).step_by(29) {
        let held = budget.reserve(budget.available() - available).unwrap();
        if let Ok(candidate) = map.with_remove(&31) {
            assert_eq!(candidate.len(), 63);
            assert!(candidate.get(&31).is_none());
            assert_eq!(verify(&candidate.root).0, 63);
        }
        drop(held);
        assert!(map.iter().eq(original.iter()));
        assert_eq!(budget.used(), baseline);
    }
    drop((original, map));
    assert_eq!(budget.used(), 0);
}

#[test]
fn borrowed_removal_and_empty_successors_need_no_additional_storage() {
    let budget = MemoryBudget::new(4096);
    let mut map = BudgetedSharedMap::new(&budget);
    map.try_insert("entry".to_owned(), 7).unwrap();
    let held = budget.reserve(budget.available()).unwrap();
    let allocation = allocation_counter::measure(|| {
        let empty = map.with_remove("entry").unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.snapshot().structural_bytes().unwrap(), 0);
        assert!(empty.snapshot().iter().next().is_none());
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(map.get("entry"), Some(&7));
    drop((held, map));
    assert_eq!(budget.used(), 0);
}

#[test]
fn large_immutable_removal_and_snapshot_iteration_have_bounded_work_and_allocation() {
    #[derive(Eq, PartialEq)]
    struct Key(usize);
    static COMPARISONS: AtomicUsize = AtomicUsize::new(0);
    impl Ord for Key {
        fn cmp(&self, other: &Self) -> Ordering {
            COMPARISONS.fetch_add(1, AtomicOrdering::Relaxed);
            self.0.cmp(&other.0)
        }
    }
    impl PartialOrd for Key {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }
    let budget = MemoryBudget::new(16 << 20);
    let mut map = BudgetedSharedMap::new(&budget);
    for key in 0..32_768 {
        map.try_insert(Key(key), key * 3).unwrap();
    }
    let selected = 16_384;
    let original = map.snapshot();
    let held = budget.reserve(budget.available() - 8192).unwrap();
    COMPARISONS.store(0, AtomicOrdering::Relaxed);
    let mut successor = None;
    let allocation = allocation_counter::measure(|| {
        successor = Some(map.with_remove(&Key(selected)).unwrap());
    });
    assert!(allocation.bytes_total <= 8192, "{allocation:?}");
    assert!(COMPARISONS.load(AtomicOrdering::Relaxed) <= 64);
    let successor = successor.unwrap();
    assert_eq!(verify(&successor.root).0, 32_767);
    assert!(successor
        .iter()
        .map(|(key, value)| (key.0, *value))
        .eq((0..32_768)
            .filter(|key| *key != selected)
            .map(|key| (key, key * 3))));
    let allocation = allocation_counter::measure(|| {
        let mut expected = 0;
        for (key, value) in &original {
            assert_eq!((key.0, *value), (expected, expected * 3));
            expected += 1;
        }
        assert_eq!(expected, 32_768);
    });
    assert_eq!(allocation.bytes_total, 0);
    drop((held, original, map, successor));
    assert_eq!(budget.used(), 0);
}
