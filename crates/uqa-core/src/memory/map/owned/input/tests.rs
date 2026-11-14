//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::{BudgetedString, MemoryBudget};
use std::{
    collections::BTreeMap,
    pin::Pin,
    task::{Context, Poll, Waker},
};

#[derive(Default)]
struct Input {
    steps: usize,
    fail: Option<usize>,
    unwind: Option<usize>,
}

impl Input {
    async fn step(&mut self) -> Result<(), MemoryError> {
        let mut yielded = false;
        std::future::poll_fn(|_| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                Poll::Pending
            }
        })
        .await;
        self.steps += 1;
        assert_ne!(self.unwind, Some(self.steps), "abandoned map comparison");
        if self.fail == Some(self.steps) {
            Err(MemoryError::SizeOverflow)
        } else {
            Ok(())
        }
    }
}

impl MapKeyInput<i32> for Input {
    type Error = MemoryError;
    async fn checkpoint(&mut self) -> Result<(), MemoryError> {
        self.step().await
    }
    async fn compare(&mut self, query: &i32, stored: &i32) -> Result<Ordering, MemoryError> {
        self.step().await?;
        Ok(query.cmp(stored))
    }
}

fn finish<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    for _ in 0..100_000 {
        if let Poll::Ready(value) = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            return value;
        }
    }
    panic!("native map input stopped progressing")
}

#[test]
fn retained_map_comparisons_preserve_order_balancing_and_original_entry_ownership() {
    let budget = MemoryBudget::new(1 << 20);
    let mut map = OwnedMap::new();
    let mut expected = BTreeMap::new();
    let mut input = Input::default();
    for key in (0..257)
        .chain((0..257).rev())
        .chain((0..257).map(|n| (n * 19) % 257))
    {
        let value = input.steps;
        let previous = finish(map.insert_admitted_key_from(
            Budgeted::new(key, budget.empty_reservation()),
            value,
            &mut input,
        ))
        .unwrap();
        assert_eq!(previous, expected.insert(key, value));
        assert!(map.iter().eq(expected.iter()));
        assert_eq!(super::super::super::tests::verify(&map.root).0, map.len());
        assert_eq!(
            budget.used(),
            map.len() * OwnedMap::<i32, usize>::entry_bytes()
        );
    }
    let used = budget.used();
    for key in -1..=258 {
        assert_eq!(
            finish(map.get_from(&key, &mut input)).unwrap(),
            expected.get(&key)
        );
    }
    assert_eq!(budget.used(), used);
    let pointer = std::ptr::from_ref(map.get(&128).unwrap());
    let occupied = budget.reserve(budget.available()).unwrap();
    finish(map.insert_admitted_key_from(
        Budgeted::new(128, budget.empty_reservation()),
        999,
        &mut input,
    ))
    .unwrap();
    assert_eq!(std::ptr::from_ref(map.get(&128).unwrap()), pointer);
    assert_eq!(map.get(&128), Some(&999));
    drop((map, occupied));
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_mutable_map_selection_preserves_keys_nodes_and_original_allocation() {
    let budget = MemoryBudget::new(1 << 20);
    let mut map = OwnedMap::new();
    for key in 0..257 {
        map.insert_budgeted(key, key * 2, &budget).unwrap();
    }
    let used = budget.used();
    let mut input = Input::default();
    for key in -1..=258 {
        let pointer = map.get(&key).map(std::ptr::from_ref);
        let selected = finish(map.get_mut_from(&key, &mut input)).unwrap();
        assert_eq!(
            selected.as_ref().map(|value| std::ptr::from_ref(&**value)),
            pointer
        );
        if let Some(value) = selected {
            *value += 1;
        }
        assert_eq!(budget.used(), used);
    }
    assert!(map.iter().all(|(key, value)| *value == key * 2 + 1));
    assert_eq!(super::super::super::tests::verify(&map.root).0, map.len());
    drop(map);
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_mutable_map_selection_failure_and_abandonment_preserve_the_complete_owner() {
    let budget = MemoryBudget::new(1 << 20);
    let mut map = OwnedMap::new();
    for key in 0..63 {
        map.insert_budgeted(key, key, &budget).unwrap();
    }
    let used = budget.used();
    for key in [32, 70] {
        let mut preview = Input::default();
        finish(map.get_mut_from(&key, &mut preview)).unwrap();
        for step in 1..=preview.steps {
            for fail in [false, true] {
                let mut input = Input {
                    fail: fail.then_some(step),
                    ..Input::default()
                };
                let future = map.get_mut_from(&key, &mut input);
                if fail {
                    assert!(finish(future).is_err());
                } else {
                    let mut future = std::pin::pin!(future);
                    for _ in 0..step {
                        assert!(future
                            .as_mut()
                            .poll(&mut Context::from_waker(Waker::noop()))
                            .is_pending());
                    }
                }
                assert!(map.iter().all(|(key, value)| key == value));
                assert_eq!(budget.used(), used);
            }
        }
    }
    drop(map);
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_map_comparison_failure_abandonment_and_denial_leave_selected_root_unchanged() {
    for key in [32, 63] {
        for outcome in ["fail", "drop", "unwind", "memory"] {
            let budget = MemoryBudget::new(1 << 20);
            let mut map = OwnedMap::new();
            for number in 0..63 {
                map.insert_budgeted(number, number * 2, &budget).unwrap();
            }
            let expected = map
                .iter()
                .map(|(key, value)| (*key, *value))
                .collect::<Vec<_>>();
            let root = std::ptr::from_ref(&*map.root.as_ref().unwrap().value);
            let used = budget.used();
            let mut complete = Input::default();
            let mut preview: OwnedMap<_, _> = expected.iter().copied().collect();
            finish(preview.insert_admitted_key_from(
                Budgeted::new(key, budget.empty_reservation()),
                -1,
                &mut complete,
            ))
            .unwrap();
            drop(preview);
            for step in 1..=complete.steps {
                let mut input = Input {
                    fail: (outcome == "fail").then_some(step),
                    unwind: (outcome == "unwind").then_some(step),
                    ..Input::default()
                };
                let occupied =
                    (outcome == "memory").then(|| budget.reserve(budget.available()).unwrap());
                let future = map.insert_admitted_key_from(
                    Budgeted::new(key, budget.empty_reservation()),
                    -1,
                    &mut input,
                );
                match outcome {
                    "drop" => {
                        let mut future = std::pin::pin!(future);
                        for _ in 0..step {
                            assert!(future
                                .as_mut()
                                .poll(&mut Context::from_waker(Waker::noop()))
                                .is_pending());
                        }
                    }
                    "unwind" => assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                        || finish(future)
                    ))
                    .is_err()),
                    "memory" if key == 32 => {
                        assert_eq!(finish(future).unwrap(), Some(64));
                        map.insert(32, 64);
                    }
                    _ => assert!(finish(future).is_err()),
                }
                drop(occupied);
                assert_eq!(budget.used(), used);
                assert_eq!(std::ptr::from_ref(&*map.root.as_ref().unwrap().value), root);
                assert!(map
                    .iter()
                    .map(|(key, value)| (*key, *value))
                    .eq(expected.iter().copied()));
            }
            drop(map);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[derive(Eq, PartialEq)]
struct Key(String);
impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Key {
    fn cmp(&self, _: &Self) -> Ordering {
        panic!("hidden synchronous key comparison")
    }
}
impl MapKeyInput<Key> for Input {
    type Error = MemoryError;
    async fn checkpoint(&mut self) -> Result<(), MemoryError> {
        self.step().await
    }
    async fn compare(&mut self, query: &Key, stored: &Key) -> Result<Ordering, MemoryError> {
        self.step().await?;
        Ok(query.0.cmp(&stored.0))
    }
}

fn key(text: &str, budget: &MemoryBudget) -> Budgeted<Key> {
    let mut text_value = BudgetedString::new(budget);
    text_value.push_str(text).unwrap();
    let (value, memory) = text_value.into_parts();
    Budgeted::new(Key(value), memory)
}

#[test]
fn retained_map_publication_never_repeats_comparisons_or_replaces_key_leases() {
    let original = MemoryBudget::new(1 << 20);
    let replacement = MemoryBudget::new(1 << 20);
    let mut map = OwnedMap::new();
    let mut input = Input::default();
    let first = key("alpha", &original);
    let pointer = first.0.as_ptr();
    for entry in [first, key("omega", &original), key("middle", &original)] {
        finish(map.insert_admitted_key_from(entry, 1, &mut input)).unwrap();
    }
    let used = original.used();
    let occupied = original.reserve(original.available()).unwrap();
    let result =
        finish(map.insert_admitted_key_from(key("alpha", &replacement), 2, &mut input)).unwrap();
    assert_eq!(result, Some(1));
    assert_eq!(map.iter().next().unwrap().0 .0.as_ptr(), pointer);
    assert_eq!(replacement.used(), 0);
    assert_eq!(original.used(), used + occupied.bytes());
    let query = key("alpha", &replacement);
    let measured = {
        let mut future = std::pin::pin!(map.get_from(&*query, &mut input));
        allocation_counter::measure(|| {
            while Pin::as_mut(&mut future)
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
            {}
        })
    };
    assert_eq!(measured.count_total, 0);
    let measured = {
        let mut future = std::pin::pin!(map.get_mut_from(&*query, &mut input));
        allocation_counter::measure(|| {
            while Pin::as_mut(&mut future)
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
            {}
        })
    };
    assert_eq!(measured.count_total, 0);
    drop((map, occupied, query));
    assert_eq!(original.used(), 0);
    assert_eq!(replacement.used(), 0);
}
