//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Entry;

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
