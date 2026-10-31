//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ValueRetentionError;
use proptest::prelude::*;
use std::{
    collections::{hash_map::RandomState, BTreeMap},
    hash::Hasher,
};

#[derive(Debug, Eq, PartialEq)]
struct Collision(u64);
impl Hash for Collision {
    fn hash<S: Hasher>(&self, state: &mut S) {
        0_u8.hash(state);
    }
}

fn inline<T>(value: T, budget: &MemoryBudget) -> Budgeted<T> {
    Budgeted::new(value, budget.empty_reservation())
}

proptest! {
    #[test]
    fn colliding_index_lookup_and_replacement_match_an_independent_map(entries in prop::collection::vec((any::<u64>(), any::<u64>()), 0..96)) {
        let budget = MemoryBudget::new(1 << 20);
        let expected: BTreeMap<_, _> = entries.iter().copied().collect();
        let index = BudgetedHashIndex::build(
            &budget,
            RandomState::new(),
            entries.iter().map(|&(key, value)| Ok::<_, MemoryError>((inline(Collision(key), &budget), inline(value, &budget)))),
            || Ok::<_, MemoryError>(()),
        ).unwrap();
        prop_assert_eq!(index.len(), expected.len());
        prop_assert_eq!(index.is_empty(), expected.is_empty());
        for (key, value) in &expected {
            prop_assert_eq!(index.get(&Collision(*key)), Some(value));
        }
        let observed: BTreeMap<_, _> = index.iter().map(|(key, value)| (key.0, *value)).collect();
        prop_assert_eq!(observed, expected);
        prop_assert_eq!(budget.used(), index.bucket_bytes());
        drop(index);
        prop_assert_eq!(budget.used(), 0);
    }
}

#[test]
fn keys_and_replaced_values_retain_only_their_live_payload_leases() {
    let budget = MemoryBudget::new(1 << 20);
    let names = ["first", "second", "first"];
    let lengths = [11, 19, 31];
    let index = BudgetedHashIndex::build(
        &budget,
        RandomState::new(),
        names.into_iter().zip(lengths).map(|(name, length)| {
            let key_memory = budget.reserve(name.len())?;
            let key = Budgeted::new(name.to_owned(), key_memory);
            let value_memory = budget.reserve(length)?;
            let value = Budgeted::new(vec![0_u8; length], value_memory);
            Ok::<_, MemoryError>((key, value))
        }),
        || Ok::<_, MemoryError>(()),
    )
    .unwrap();
    assert_eq!(index.get("first").unwrap().len(), 31);
    assert_eq!(index.get("second").unwrap().len(), 19);
    assert_eq!(
        budget.used(),
        index.bucket_bytes() + "firstsecond".len() + 31 + 19
    );
    assert!(index.get("absent").is_none());
    drop(index);
    assert_eq!(budget.used(), 0);
}

#[test]
fn rejected_bucket_growth_stops_the_lazy_producer_and_releases_partial_index() {
    let parent = MemoryBudget::new(512);
    let budget = parent.child(1 << 20);
    let sibling = parent.reserve(7).unwrap();
    let produced = std::cell::Cell::new(0);
    let allocation = allocation_counter::measure(|| {
        let result = BudgetedHashIndex::build(
            &budget,
            RandomState::new(),
            (0..1_000_000).map(|key| {
                produced.set(produced.get() + 1);
                Ok::<_, MemoryError>((inline(key, &budget), inline(key, &budget)))
            }),
            || Ok::<_, MemoryError>(()),
        );
        assert!(matches!(result, Err(MemoryError::Limit { .. })));
    });
    assert!(produced.get() < 16);
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(budget.used(), 0);
    assert_eq!(parent.used(), 7);
    drop(sibling);
}

#[test]
fn cancellation_and_unwind_during_rehash_release_both_bucket_generations() {
    for fail_at in [1, 7, 19, 43, 97, 151, 257] {
        for panic in [false, true] {
            let budget = MemoryBudget::new(1 << 20);
            let sibling = budget.reserve(7).unwrap();
            let calls = std::cell::Cell::new(0);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                BudgetedHashIndex::build(
                    &budget,
                    RandomState::new(),
                    (0..128).map(|key| {
                        Ok::<_, ValueRetentionError>((
                            inline(Collision(key), &budget),
                            inline(key, &budget),
                        ))
                    }),
                    || {
                        calls.set(calls.get() + 1);
                        if calls.get() == fail_at {
                            assert!(!panic, "producer unwind");
                            return Err(crate::QueryCancelled::USER_REQUEST.into());
                        }
                        Ok(())
                    },
                )
            }));
            if panic {
                assert!(result.is_err());
            } else {
                assert!(matches!(
                    result.unwrap(),
                    Err(ValueRetentionError::Cancelled(_))
                ));
            }
            assert_eq!(calls.get(), fail_at);
            assert_eq!(budget.used(), 7);
            drop(sibling);
        }
    }
}
