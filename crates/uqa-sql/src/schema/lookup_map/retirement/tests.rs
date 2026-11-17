//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::lookup_map::SchemaMap;
use std::sync::Barrier;
use uqa_core::{
    memory::{BudgetedHashIndex, MemoryBudget, ProductionControl, ProductionString, ProductionVec},
    CancellationToken,
};

type Index = Produced<RetainedIndex<String, Vec<u8>>>;

fn fixture(
    sorted: bool,
    count: usize,
    control: &ProductionControl<'_>,
) -> (Index, Vec<(usize, usize)>) {
    let mut addresses = Vec::new();
    let mut entries = ProductionVec::new(*control);
    for id in 0..count {
        let mut key = ProductionString::new(*control);
        key.push_str(&format!("key_{id:04}")).unwrap();
        let key = key.finish().unwrap();
        let mut bytes = ProductionVec::new(*control);
        for _ in 0..19 + id {
            bytes.push_copy(u8::try_from(id).unwrap()).unwrap();
        }
        let value = bytes.finish().unwrap();
        addresses.push((key.as_ptr() as usize, value.as_ptr() as usize));
        entries
            .push_produced(
                control
                    .finish((key, value), control.empty_reservation())
                    .unwrap(),
            )
            .unwrap();
    }
    let (entries, memory) = entries.finish().unwrap().into_parts();
    let index = if sorted {
        control
            .finish(RetainedIndex::Sorted(entries), memory)
            .unwrap()
    } else {
        let index = BudgetedHashIndex::build(
            control.budget().unwrap(),
            ahash::RandomState::default(),
            entries.into_iter().map(|(key, value)| {
                Ok::<_, crate::SQLError>((
                    key.into_budgeted().unwrap(),
                    value.into_budgeted().unwrap(),
                ))
            }),
            || control.check().map_err(crate::SQLError::from),
        )
        .unwrap();
        drop(memory);
        control
            .finish(RetainedIndex::Hash(index), control.empty_reservation())
            .unwrap()
    };
    (index, addresses)
}

#[test]
fn schema_index_retirement_preserves_original_entries_and_buffer_allowances() {
    for sorted in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let cancel = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancel, &cancel);
        let (index, addresses) = fixture(sorted, 17, &control);
        let before = budget.used();
        let exhausted = budget.reserve(budget.available()).unwrap();
        let mut slots = None;
        let allocation =
            allocation_counter::measure(|| slots = Some(RetainedIndexSlots::from(index)));
        assert_eq!(allocation.count_total, 0);
        let mut slots = slots.unwrap();
        assert_eq!(budget.used(), before + exhausted.bytes());
        let length = slots.len();
        let mut seen = [false; 17];
        let mut previous = None;
        for position in 0..length {
            let mut next = None;
            let allocation = allocation_counter::measure(|| next = slots.next());
            assert_eq!(allocation.count_total, 0);
            assert_eq!(slots.len(), length - position - 1);
            if let Some((key, value)) = next.expect("original slot") {
                let id: usize = key.strip_prefix("key_").unwrap().parse().unwrap();
                assert!(!seen[id]);
                seen[id] = true;
                assert_eq!(key.as_ptr() as usize, addresses[id].0);
                assert_eq!(value.as_ptr() as usize, addresses[id].1);
                assert!(key.is_retained_by(&budget));
                assert!(value.is_retained_by(&budget));
                assert!(value.iter().all(|byte| usize::from(*byte) == id));
                if sorted {
                    assert_eq!(id, previous.map_or(0, |id| id + 1));
                    previous = Some(id);
                }
            }
        }
        assert!(seen.into_iter().all(|seen| seen));
        assert!(slots.next().is_none());
        assert!(slots.next().is_none());
        let allocation = allocation_counter::measure(|| drop(slots));
        assert_eq!(allocation.count_total, 0);
        assert_eq!(budget.used(), exhausted.bytes());
        drop(exhausted);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn schema_index_last_owner_retires_each_partial_prefix_without_affecting_shared_readers() {
    for sorted in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let cancel = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancel, &cancel);
        let (index, _) = fixture(sorted, 17, &control);
        let shared = index.into_shared(&control).unwrap();
        let reader = SchemaMap::Admitted(shared.clone());
        let before = budget.used();
        let allocation = allocation_counter::measure(|| {
            let mut slots = RetainedIndexSlots::from(shared);
            assert_eq!(slots.len(), 0);
            assert!(slots.next().is_none());
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(budget.used(), before);
        assert_eq!(reader.get("key_0003").unwrap(), &vec![3; 22]);
        let allocation = allocation_counter::measure(|| drop(reader));
        assert_eq!(allocation.count_total, 0);
        assert_eq!(budget.used(), 0);

        let (index, _) = fixture(sorted, 17, &control);
        let length = RetainedIndexSlots::from(index).len();
        for prefix in 0..=length {
            let (index, _) = fixture(sorted, 17, &control);
            let independent = budget.reserve(37).unwrap();
            let mut slots = RetainedIndexSlots::from(index);
            for _ in 0..prefix {
                drop(slots.next().expect("original slot prefix"));
            }
            let allocation = allocation_counter::measure(|| drop(slots));
            assert_eq!(allocation.count_total, 0);
            assert_eq!(budget.used(), 37, "sorted {sorted}, prefix {prefix}");
            drop(independent);
        }
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn schema_index_entries_keep_their_original_owners_after_detached_storage_retires() {
    for sorted in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let cancel = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancel, &cancel);
        let (index, _) = fixture(sorted, 17, &control);
        let shared = index.into_shared(&control).unwrap();
        let mut slots = RetainedIndexSlots::from(shared);
        let (key, value) = slots.by_ref().flatten().next().unwrap();
        let key_bytes = key.reserved_bytes();
        let value_bytes = value.reserved_bytes();
        let allocation = allocation_counter::measure(|| drop(slots));
        assert_eq!(allocation.count_total, 0);
        assert_eq!(budget.used(), key_bytes + value_bytes);
        drop(key);
        assert_eq!(budget.used(), value_bytes);
        assert_eq!(value.len(), 19 + usize::from(value[0]));
        drop(value);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn concurrent_schema_index_retirement_selects_exactly_one_last_owner() {
    for sorted in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let cancel = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancel, &cancel);
        let (index, addresses) = fixture(sorted, 17, &control);
        let shared = index.into_shared(&control).unwrap();
        let readers = [shared.clone(), shared.clone(), shared.clone(), shared];
        let barrier = Barrier::new(readers.len());
        let observed = std::thread::scope(|scope| {
            let handles: Vec<_> = readers
                .into_iter()
                .map(|reader| {
                    let barrier = &barrier;
                    let addresses = &addresses;
                    scope.spawn(move || {
                        barrier.wait();
                        RetainedIndexSlots::from(reader)
                            .flatten()
                            .map(|(key, value)| {
                                let id: usize = key.strip_prefix("key_").unwrap().parse().unwrap();
                                assert_eq!(key.as_ptr() as usize, addresses[id].0);
                                assert_eq!(value.as_ptr() as usize, addresses[id].1);
                                id
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(observed.iter().filter(|items| !items.is_empty()).count(), 1);
        let mut entries: Vec<_> = observed.into_iter().flatten().collect();
        entries.sort_unstable();
        assert_eq!(entries, (0..17).collect::<Vec<_>>());
        assert_eq!(budget.used(), 0);
    }
}
