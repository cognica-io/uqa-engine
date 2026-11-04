//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

proptest! {
    #[test]
    fn both_ends_match_independent_ordered_map_after_source_replacement(
        entries in prop::collection::vec((0_u8..100, any::<i32>()), 0..160),
        directions in prop::collection::vec(any::<bool>(), 0..180),
    ) {
        let memory = MemoryBudget::new(1 << 20);
        let mut source = BudgetedSharedMap::new(&memory);
        let mut expected = BTreeMap::new();
        for (key, value) in entries {
            source.try_insert(key, value).unwrap();
            expected.insert(key, value);
        }
        prop_assert!(source.iter_bidirectional().eq(expected.iter()));
        prop_assert!(source.iter_bidirectional().rev().eq(expected.iter().rev()));
        let captured = source.snapshot();
        source.try_insert(255, 0).unwrap();
        drop(source);
        let used = memory.used();
        let mut actual = captured.iter_bidirectional();
        let mut oracle = expected.iter();
        for reverse in directions {
            prop_assert_eq!(actual.len(), oracle.len());
            let actual = if reverse {actual.next_back()} else {actual.next()};
            let oracle = if reverse {oracle.next_back()} else {oracle.next()};
            prop_assert_eq!(actual, oracle);
        }
        prop_assert!(actual.by_ref().eq(oracle));
        prop_assert_eq!(actual.size_hint(), (0, Some(0)));
        prop_assert!(actual.next().is_none());
        prop_assert!(actual.next_back().is_none());
        prop_assert_eq!(memory.used(), used);
        drop(captured);
        prop_assert_eq!(memory.used(), 0);
    }
}

#[test]
fn bidirectional_cursor_bounds_space_work_and_allocations() {
    #[derive(Eq, PartialEq)]
    struct Key(usize);
    static COMPARISONS: AtomicUsize = AtomicUsize::new(0);
    impl Ord for Key {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            COMPARISONS.fetch_add(1, AtomicOrdering::Relaxed);
            self.0.cmp(&other.0)
        }
    }
    impl PartialOrd for Key {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    assert!(size_of::<BudgetedSharedMapBidirectionalIter<'_, Key, usize>>() <= 64);
    for count in [0, 1, 64, 8192] {
        let memory = MemoryBudget::new(2 << 20);
        let mut source = BudgetedSharedMap::new(&memory);
        for key in 0..count {
            source.try_insert(Key(key), key * 2).unwrap();
        }
        let occupied = memory.reserve(memory.available()).unwrap();
        let usage = allocation_counter::measure(|| {
            let mut cursor = source.iter_bidirectional();
            let mut front = 0;
            let mut back = count;
            for step in 0..count {
                COMPARISONS.store(0, AtomicOrdering::Relaxed);
                let (entry, expected) = if step % 2 == 0 {
                    let expected = front;
                    front += 1;
                    (cursor.next(), expected)
                } else {
                    back -= 1;
                    (cursor.next_back(), back)
                };
                assert_eq!(
                    entry.map(|(key, value)| (key.0, *value)),
                    Some((expected, expected * 2))
                );
                assert!(COMPARISONS.load(AtomicOrdering::Relaxed) < 32);
            }
            assert!(cursor.next().is_none());
            assert!(cursor.next_back().is_none());
        });
        assert_eq!(usage.count_total, 0);
        drop((occupied, source));
        assert_eq!(memory.used(), 0);
    }
}
