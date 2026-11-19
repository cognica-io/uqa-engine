//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Debug)]
struct Key {
    id: usize,
    name: String,
    forbid_lookup: Arc<AtomicBool>,
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        assert!(!self.forbid_lookup.load(Ordering::Relaxed));
        self.id == other.id
    }
}
impl Eq for Key {}
impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        assert!(!self.forbid_lookup.load(Ordering::Relaxed));
        0_u8.hash(state);
    }
}

type Index = BudgetedHashIndex<Key, Vec<u8>, RandomState>;

fn fixture(budget: &MemoryBudget, count: usize) -> (Index, Vec<(usize, usize)>) {
    let forbid_lookup = Arc::new(AtomicBool::new(false));
    let mut addresses = Vec::new();
    let index = BudgetedHashIndex::build(
        budget,
        RandomState::new(),
        (0..count).map(|id| {
            let name = format!("original-key-{id}");
            let payload = vec![u8::try_from(id).unwrap(); 19 + id];
            addresses.push((name.as_ptr() as usize, payload.as_ptr() as usize));
            let key_memory = budget.reserve(name.capacity())?;
            let value_memory = budget.reserve(payload.capacity())?;
            Ok::<_, MemoryError>((
                Budgeted::new(
                    Key {
                        id,
                        name,
                        forbid_lookup: forbid_lookup.clone(),
                    },
                    key_memory,
                ),
                Budgeted::new(payload, value_memory),
            ))
        }),
        || Ok::<_, MemoryError>(()),
    )
    .unwrap();
    forbid_lookup.store(true, Ordering::Relaxed);
    (index, addresses)
}

#[test]
fn consuming_slots_transfer_original_entries_without_lookup_copy_or_allocation() {
    for count in [0, 1, 17, 64] {
        let budget = MemoryBudget::new(1 << 20);
        let (index, addresses) = fixture(&budget, count);
        let buckets = index.buckets.len();
        let bucket_bytes = index.bucket_bytes();
        let before = budget.used();
        let exhausted = budget.reserve(budget.available()).unwrap();
        let mut slots = None;
        let allocation = allocation_counter::measure(|| slots = Some(index.into_slots()));
        assert_eq!(allocation.count_total, 0);
        let mut slots = slots.unwrap();
        assert_eq!(slots.len(), buckets);
        assert_eq!(slots.remaining_entries(), count);
        assert_eq!(budget.used(), before + exhausted.bytes());
        let mut seen = vec![false; count];
        let mut occupied = 0;
        for position in 0..buckets {
            let mut next = None;
            let allocation = allocation_counter::measure(|| next = slots.next());
            assert_eq!(allocation.count_total, 0);
            assert_eq!(
                slots.size_hint(),
                (buckets - position - 1, Some(buckets - position - 1))
            );
            if let Some((key, value)) = next.expect("one original bucket") {
                assert!(!seen[key.id]);
                seen[key.id] = true;
                assert_eq!(key.name.as_ptr() as usize, addresses[key.id].0);
                assert_eq!(value.as_ptr() as usize, addresses[key.id].1);
                assert!(value.iter().all(|byte| usize::from(*byte) == key.id));
                occupied += 1;
            }
            assert_eq!(slots.remaining_entries(), count - occupied);
        }
        assert!(seen.into_iter().all(|value| value));
        assert!(slots.next().is_none());
        assert!(slots.next().is_none());
        assert_eq!(budget.used(), bucket_bytes + exhausted.bytes());
        drop(slots);
        assert_eq!(budget.used(), exhausted.bytes());
        drop(exhausted);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn transferred_key_and_value_keep_independent_allowances_after_cursor_drop() {
    let budget = MemoryBudget::new(1 << 20);
    let (index, addresses) = fixture(&budget, 17);
    let independent = budget.reserve(37).unwrap();
    let mut slots = index.into_slots();
    let (key, value) = slots.by_ref().flatten().next().unwrap();
    let key_bytes = key.name.capacity();
    let value_bytes = value.capacity();
    let id = key.id;
    let allocation = allocation_counter::measure(|| drop(slots));
    assert_eq!(allocation.count_total, 0);
    assert_eq!(budget.used(), 37 + key_bytes + value_bytes);
    assert_eq!(key.name.as_ptr() as usize, addresses[id].0);
    assert_eq!(value.as_ptr() as usize, addresses[id].1);
    drop(key);
    assert_eq!(budget.used(), 37 + value_bytes);
    assert!(value.iter().all(|byte| usize::from(*byte) == id));
    drop(value);
    assert_eq!(budget.used(), 37);
    drop(independent);
    assert_eq!(budget.used(), 0);
}

#[test]
fn draining_detaches_the_layout_and_leaves_a_valid_empty_index() {
    let budget = MemoryBudget::new(1 << 20);
    let (mut index, _) = fixture(&budget, 17);
    let before = budget.used();
    let mut slots = None;
    let allocation = allocation_counter::measure(|| slots = Some(index.drain_slots()));
    assert_eq!(allocation.count_total, 0);
    let slots = slots.unwrap();
    assert!(index.is_empty());
    assert_eq!(index.len(), 0);
    assert_eq!(index.bucket_bytes(), 0);
    assert_eq!(index.iter().len(), 0);
    assert!(index.drain_slots().next().is_none());
    assert_eq!(slots.remaining_entries(), 17);
    drop(index);
    assert_eq!(budget.used(), before);
    drop(slots);
    assert_eq!(budget.used(), 0);
}

#[test]
fn abandoning_each_bucket_prefix_releases_only_the_cursor_owned_storage() {
    let budget = MemoryBudget::new(1 << 20);
    let (index, _) = fixture(&budget, 17);
    let buckets = index.buckets.len();
    drop(index);
    for prefix in 0..=buckets {
        let (index, _) = fixture(&budget, 17);
        let independent = budget.reserve(37).unwrap();
        let mut slots = index.into_slots();
        for _ in 0..prefix {
            drop(slots.next().expect("original bucket prefix"));
        }
        let allocation = allocation_counter::measure(|| drop(slots));
        assert_eq!(allocation.count_total, 0);
        assert_eq!(budget.used(), 37, "prefix {prefix}");
        drop(independent);
        assert_eq!(budget.used(), 0);
    }
}
