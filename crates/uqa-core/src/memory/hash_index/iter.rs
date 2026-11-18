//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Entry;
use crate::memory::{Budgeted, MemoryReservation};

/// Borrowed traversal of completed index entries; no cursor allocation or payload copy is required.
pub struct BudgetedHashIndexIter<'a, K, V> {
    pub(super) buckets: std::slice::Iter<'a, Option<Entry<K, V>>>,
    pub(super) remaining: usize,
}

impl<'a, K, V> Iterator for BudgetedHashIndexIter<'a, K, V> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.buckets.by_ref().flatten().next()?;
        self.remaining -= 1;
        Some((&entry.key, &entry.value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl<K, V> ExactSizeIterator for BudgetedHashIndexIter<'_, K, V> {}
impl<K, V> std::iter::FusedIterator for BudgetedHashIndexIter<'_, K, V> {}

/// Consuming traversal of the original bucket layout. `Some(None)` is an empty bucket; `None` means the entire layout has been consumed. No hashing, equality, cloning or allocation occurs when creating or advancing this cursor. Its size hint counts remaining buckets, not entries.
pub struct BudgetedHashIndexSlots<K, V> {
    pub(super) buckets: std::vec::IntoIter<Option<Entry<K, V>>>,
    pub(super) remaining: usize,
    // Free the original buffer and every unconsumed entry before returning its bucket allowance.
    pub(super) _memory: MemoryReservation,
}

impl<K, V> BudgetedHashIndexSlots<K, V> {
    pub fn remaining_entries(&self) -> usize {
        self.remaining
    }
}

impl<K, V> Iterator for BudgetedHashIndexSlots<K, V> {
    type Item = Option<(Budgeted<K>, Budgeted<V>)>;

    fn next(&mut self) -> Option<Self::Item> {
        self.buckets.next().map(|bucket| {
            bucket.map(|Entry { key, value, .. }| {
                self.remaining -= 1;
                (key, value)
            })
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.buckets.size_hint()
    }
}

impl<K, V> ExactSizeIterator for BudgetedHashIndexSlots<K, V> {}
impl<K, V> std::iter::FusedIterator for BudgetedHashIndexSlots<K, V> {}
