//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::{MemoryError, RetainedMap};

#[derive(PartialEq, Eq)]
struct Key(usize);
impl Ord for Key {
    fn cmp(&self, _: &Self) -> Ordering {
        panic!("native insertion must not repeat an ordinary key comparison")
    }
}
impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl MapKeyInput<Key> for Input {
    type Error = MemoryError;

    async fn checkpoint(&mut self) -> Result<(), MemoryError> {
        <Self as MapKeyInput<Query, usize>>::checkpoint(self)
            .await
            .map_err(|_| MemoryError::SizeOverflow)
    }

    async fn compare(&mut self, query: &Key, stored: &Key) -> Result<Ordering, MemoryError> {
        <Self as MapKeyInput<Key>>::checkpoint(self).await?;
        Ok(query.0.cmp(&stored.0))
    }
}

fn input(fail_at: Option<usize>) -> Input {
    Input {
        visited: 0,
        fail_at,
    }
}

#[test]
fn retained_map_insertion_preserves_order_snapshots_and_exact_footprints_without_ordinary_comparisons(
) {
    let memory = MemoryBudget::new(1 << 20);
    let capture = MemoryBudget::new(1 << 20);
    let mut map = RetainedMap::new(&memory);
    let reference_memory = MemoryBudget::new(1 << 20);
    let mut reference = RetainedMap::new(&reference_memory);
    let mut expected = BTreeMap::new();
    let mut snapshots = Vec::new();
    for (index, key) in (0..96usize).map(|index| (index, (index * 37) % 64)) {
        if index % 13 == 0 {
            snapshots.push((map.clone(), expected.clone()));
        }
        finish(map.prepare_insert_from(Key(key), index + 1, &mut input(None)))
            .unwrap()
            .publish(index);
        reference.insert(key, index, index + 1).unwrap();
        expected.insert(key, index);
        assert_eq!(map.len(), expected.len());
        assert!(map
            .iter()
            .map(|(key, value)| (key.0, *value))
            .eq(expected.iter().map(|(&key, &value)| (key, value))));
        let selected = map.capture(&capture).unwrap();
        assert_eq!(
            selected.reserved_bytes(),
            reference.retained_bytes().unwrap()
        );
        drop(selected);
        assert_eq!(capture.used(), 0);
    }
    for (snapshot, expected) in &snapshots {
        assert!(snapshot
            .iter()
            .map(|(key, value)| (key.0, *value))
            .eq(expected.iter().map(|(&key, &value)| (key, value))));
    }
    drop((map, snapshots, reference));
    assert_eq!(memory.used(), 0);
    assert_eq!(reference_memory.used(), 0);
}

#[test]
fn retained_map_insertion_failure_and_each_suspended_retirement_preserve_original_generations() {
    let memory = MemoryBudget::new(1 << 20);
    let mut source = RetainedMap::new(&memory);
    for key in 0..32usize {
        finish(source.prepare_insert_from(Key(key), key + 1, &mut input(None)))
            .unwrap()
            .publish(key);
    }
    for key in [0, 15, 31, 44] {
        let mut candidate = source.clone();
        let mut complete = input(None);
        let pending = finish(candidate.prepare_insert_from(Key(key), 99, &mut complete)).unwrap();
        drop(pending);
        let before = memory.used();
        for point in 0..complete.visited {
            let mut candidate = source.clone();
            assert!(
                finish(candidate.prepare_insert_from(Key(key), 99, &mut input(Some(point))))
                    .is_err()
            );
            assert!(candidate.shares_root(&source));
            assert_eq!(
                candidate.retained_bytes().unwrap(),
                source.retained_bytes().unwrap()
            );
            assert_eq!(memory.used(), before);
            let mut work = input(None);
            {
                let future = candidate.prepare_insert_from(Key(key), 99, &mut work);
                let mut future = std::pin::pin!(future);
                for _ in 0..=point {
                    assert!(future
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending());
                }
            }
            assert!(candidate.shares_root(&source));
            assert_eq!(
                candidate.retained_bytes().unwrap(),
                source.retained_bytes().unwrap()
            );
            assert_eq!(memory.used(), before);
        }
        let hold = memory.reserve(memory.available()).unwrap();
        assert!(matches!(
            finish(candidate.prepare_insert_from(Key(key), 99, &mut input(None))),
            Err(MemoryError::Limit { .. })
        ));
        assert!(candidate.shares_root(&source));
        drop(hold);
        assert_eq!(memory.used(), before);
    }
    let before = memory.used();
    let hold = memory.reserve(memory.available()).unwrap();
    finish(source.prepare_insert_from(Key(15), 1000, &mut input(None)))
        .unwrap()
        .publish(1000);
    drop(hold);
    assert_eq!(memory.used(), before);
    assert_eq!(*source.iter().find(|(key, _)| key.0 == 15).unwrap().1, 1000);
    drop(source);
    assert_eq!(memory.used(), 0);
}
