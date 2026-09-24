//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{collections::BTreeSet, hash::Hasher};

use proptest::prelude::*;

use super::*;

#[derive(Debug, PartialEq, Eq)]
struct Collision(u64);

impl Hash for Collision {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        0_u64.hash(hasher);
    }
}

proptest! {
    #[test]
    fn colliding_membership_matches_an_independent_ordered_set(values in prop::collection::vec(any::<u64>(), 0..160)) {
        let budget = MemoryBudget::new(1 << 20);
        let mut actual = BudgetedHashSet::new(&budget);
        let mut expected = BTreeSet::new();
        for value in values {
            prop_assert_eq!(actual.insert(Collision(value)).unwrap(), expected.insert(value));
            prop_assert_eq!(actual.len(), expected.len());
            prop_assert_eq!(actual.is_empty(), expected.is_empty());
            for member in &expected {
                prop_assert!(actual.contains(&Collision(*member)));
            }
        }
        drop(actual);
        prop_assert_eq!(budget.used(), 0);
    }
}

#[test]
fn failed_growth_preserves_members_and_duplicate_insertion_needs_no_headroom() {
    let bucket_bytes = size_of::<Option<Entry<u64>>>();
    let budget = MemoryBudget::new(5 * bucket_bytes);
    let mut set = BudgetedHashSet::new(&budget);
    set.insert(u64::MAX).unwrap();
    let retained = budget.used();
    assert!(matches!(set.insert(0), Err(MemoryError::Limit { .. })));
    assert!(set.contains(&u64::MAX));
    assert!(!set.contains(&0));
    assert_eq!(set.len(), 1);
    assert_eq!(budget.used(), retained);
    let full = budget.reserve(budget.limit() - retained).unwrap();
    assert!(!set.insert(u64::MAX).unwrap());
    assert!(matches!(
        set.reserve(usize::MAX),
        Err(MemoryError::SizeOverflow)
    ));
    assert_eq!(set.len(), 1);
    drop(full);
    drop(set);
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_bucket_layouts_stay_charged_while_the_replacement_coexists() {
    let budget = MemoryBudget::new(1 << 20);
    let mut set = BudgetedHashSet::new(&budget);
    set.insert(1_u64).unwrap();
    let old = budget.used();
    set.insert(2).unwrap();
    let current = set.buckets.capacity() * size_of::<Option<Entry<u64>>>();
    assert_eq!(budget.used(), current);
    assert_eq!(budget.peak(), old + current);
    drop(set);
    assert_eq!(budget.used(), 0);
}

#[test]
fn resizing_moves_owned_values_without_hashing_them_again() {
    use std::{cell::Cell, rc::Rc};

    struct Key {
        id: usize,
        calls: Rc<Cell<usize>>,
    }
    impl PartialEq for Key {
        fn eq(&self, other: &Self) -> bool {
            self.id == other.id
        }
    }
    impl Eq for Key {}
    impl Hash for Key {
        fn hash<H: Hasher>(&self, hasher: &mut H) {
            self.calls.set(self.calls.get() + 1);
            self.id.hash(hasher);
        }
    }
    let budget = MemoryBudget::new(1 << 20);
    let calls = Rc::new(Cell::new(0));
    let mut set = BudgetedHashSet::new(&budget);
    for id in 0..64 {
        set.insert(Key {
            id,
            calls: Rc::clone(&calls),
        })
        .unwrap();
    }
    assert_eq!(calls.get(), 64);
    set.reserve(512).unwrap();
    assert_eq!(calls.get(), 64);
    drop(set);
    assert_eq!(Rc::strong_count(&calls), 1);
    assert_eq!(budget.used(), 0);
}
