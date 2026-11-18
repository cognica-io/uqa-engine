//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    cmp::Ordering,
    collections::BTreeMap,
    future::Future,
    task::{Context, Poll, Waker},
};

#[derive(Default)]
struct Input {
    steps: usize,
    fail: Option<usize>,
}

impl MapKeyInput<i32> for Input {
    type Error = ValueRetentionError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        let mut pending = true;
        std::future::poll_fn(|_| {
            if std::mem::take(&mut pending) {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        self.steps += 1;
        if self.fail == Some(self.steps) {
            return Err(MemoryError::SizeOverflow.into());
        }
        Ok(())
    }
    async fn compare(&mut self, left: &i32, right: &i32) -> Result<Ordering, Self::Error> {
        self.checkpoint().await?;
        Ok(left.cmp(right))
    }
}

fn complete<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    for _ in 0..10_000 {
        if let Poll::Ready(value) = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            return value;
        }
    }
    panic!("retained production map stopped progressing")
}

#[test]
fn retained_production_map_shares_nodes_with_inline_insertion_and_lookup() {
    for controlled in [false, true] {
        let budget = MemoryBudget::new(257 * OwnedMap::<i32, i32>::entry_bytes());
        let token = CancellationToken::new();
        let control = if controlled {
            ProductionControl::new(&budget, &token, &token)
        } else {
            ProductionControl::uncontrolled()
        };
        let mut map = ProductionMap::new(control);
        let mut expected = BTreeMap::new();
        let mut input = Input::default();
        for (index, key) in (0..257).chain((0..257).rev()).enumerate() {
            let value = key * 2 + i32::try_from(index).unwrap();
            let previous = if index % 2 == 0 {
                map.insert_copy(key, value).unwrap()
            } else {
                complete(map.insert_copy_from(key, value, &mut input)).unwrap()
            };
            assert_eq!(previous, expected.insert(key, value));
            assert!(map.iter().eq(expected.iter()));
            assert_eq!(
                budget.used(),
                if controlled {
                    map.len() * OwnedMap::<i32, i32>::entry_bytes()
                } else {
                    0
                }
            );
        }
        for key in -1..=258 {
            assert_eq!(
                complete(map.get_from(&key, &mut input)).unwrap(),
                expected.get(&key)
            );
        }
        let pointer = std::ptr::from_ref(map.get(&128).unwrap());
        complete(map.insert_copy_from(128, -1, &mut input)).unwrap();
        assert_eq!(std::ptr::from_ref(map.get(&128).unwrap()), pointer);
        assert_eq!(map.remove(&128), Some(-1));
        drop(map);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn interrupted_production_map_admission_preserves_entries_and_both_cancellation_scopes() {
    for key in [16, 40] {
        for outcome in ["drop", "failure", "original", "invoking"] {
            for prefix in 0..12 {
                let budget = MemoryBudget::new(1 << 20);
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut map =
                    ProductionMap::new(ProductionControl::new(&budget, &original, &invoking));
                for key in 0..31 {
                    map.insert_copy(key, key).unwrap();
                }
                let held = budget.used();
                let mut input = Input {
                    fail: (outcome == "failure").then_some(prefix.max(1)),
                    ..Input::default()
                };
                let completed = {
                    let mut future = std::pin::pin!(map.insert_copy_from(key, -1, &mut input));
                    let mut completed = false;
                    for _ in 0..prefix {
                        if future
                            .as_mut()
                            .poll(&mut Context::from_waker(Waker::noop()))
                            .is_ready()
                        {
                            completed = true;
                            break;
                        }
                    }
                    if !completed {
                        match outcome {
                            "original" => original.cancel(),
                            "invoking" => invoking.cancel(),
                            _ => {}
                        }
                        if outcome != "drop" {
                            assert!(
                                complete(future.as_mut()).is_err(),
                                "{key}: {outcome} at prefix {prefix}"
                            );
                        }
                    }
                    completed
                };
                if !completed {
                    assert!(map.iter().all(|(key, value)| key == value));
                    assert_eq!(map.len(), 31);
                    assert_eq!(budget.used(), held);
                }
                let allocation = allocation_counter::measure(|| drop(map));
                assert_eq!(allocation.count_total, 0);
                assert_eq!(budget.used(), 0);
            }
        }
    }
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let mut map = ProductionMap::new(ProductionControl::new(&budget, &token, &token));
    let allocation = allocation_counter::measure(|| {
        assert!(matches!(
            complete(map.insert_copy_from(1_i32, 2_i32, &mut Input::default())),
            Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
        ));
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(budget.used(), 0);
}
