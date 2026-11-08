//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn lower_bound_ranges_match_ordered_reference_without_allocating_or_retaining_leases() {
    let budget = MemoryBudget::new(1 << 20);
    let mut map = OwnedMap::new();
    let mut reference = BTreeMap::new();
    for key in [u64::MAX, 7, 1, 99, 0, 3, 1000] {
        map.insert_budgeted(key, key, &budget).unwrap();
        reference.insert(key, key);
    }
    let admitted = budget.used();
    for key in [0, 1, 2, 3, 7, 8, 99, 100, 1000, u64::MAX] {
        for bound in [
            Bound::Included(&key),
            Bound::Excluded(&key),
            Bound::Unbounded,
        ] {
            let measured = allocation_counter::measure(|| {
                let mut actual = map.range_from(bound);
                let mut expected = reference.range((bound, Bound::Unbounded));
                while let Some(next) = expected.next() {
                    assert!(actual.size_hint().1.unwrap() > expected.clone().count());
                    assert_eq!(actual.next(), Some(next));
                }
                assert_eq!(actual.next(), None);
                assert_eq!(actual.size_hint(), (0, Some(0)));
                assert_eq!(actual.next(), None);
            });
            assert_eq!(measured.count_total, 0);
            assert_eq!(budget.used(), admitted);
        }
    }
    map.clear();
    assert_eq!(map.range_from(Bound::<&u64>::Unbounded).next(), None);
    assert_eq!(budget.used(), 0);
}
