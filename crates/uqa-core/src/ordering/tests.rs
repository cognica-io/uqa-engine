//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::{Budgeted, MemoryBudget};

#[test]
fn fallible_order_matches_slice_order_across_duplicates_and_heap_boundaries() {
    for seed in 0..512_u64 {
        let mut state = seed + 1;
        let mut values = (0..seed % 129)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                state % 19
            })
            .collect::<Vec<_>>();
        let mut expected = values.clone();
        expected.sort_unstable();
        sort_by_with_control(&mut values, &mut || Ok::<(), ()>(()), |left, right, _| {
            Ok(left.cmp(right))
        })
        .unwrap();
        assert_eq!(values, expected, "seed={seed}");
    }
}

#[test]
fn every_sort_callback_failure_keeps_elements_and_their_unique_reservations() {
    let budget = MemoryBudget::new(1000);
    let original: Vec<_> = (0..19).rev().collect();
    let mut calls = 0;
    let mut values = original.clone();
    sort_by_with_control(
        &mut values,
        &mut || {
            calls += 1;
            Ok::<(), ()>(())
        },
        |left, right, poll| {
            poll()?;
            Ok(left.cmp(right))
        },
    )
    .unwrap();
    for stop in 1..=calls {
        let other = budget.reserve(7).unwrap();
        let mut values = original
            .iter()
            .map(|&value| Budgeted::new(value, budget.reserve(1).unwrap()))
            .collect::<Vec<_>>();
        let mut count = 0;
        let result = sort_by_with_control(
            &mut values,
            &mut || {
                count += 1;
                if count == stop {
                    Err(stop)
                } else {
                    Ok(())
                }
            },
            |left, right, poll| {
                poll()?;
                Ok((**left).cmp(&**right))
            },
        );
        assert_eq!(result, Err(stop));
        assert_eq!(budget.used(), original.len() + 7);
        let mut retained: Vec<_> = values.iter().map(|value| **value).collect();
        retained.sort_unstable();
        assert_eq!(retained, (0..19).collect::<Vec<_>>());
        drop(values);
        assert_eq!(budget.used(), 7);
        drop(other);
    }
}
