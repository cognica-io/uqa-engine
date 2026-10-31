//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary ordered owners expose exact allocation layouts for later immutable retention.

mod iter_mut;
mod serialization;
mod set;
#[cfg(test)]
mod tests;

pub use set::{OwnedSet, OwnedSetIntoIter, OwnedSetIter};

use std::{borrow::Borrow, ops::Bound};

use super::{tree, BudgetedMapIter, Link, Node, OwnedNode, MAX_HEIGHT};
use crate::memory::{MemoryBudget, MemoryError};

/// A mutable ordered map with exact node ownership. Ordinary insertion leaves admission to its enclosing owner; `insert_budgeted` admits and retains each new node under the supplied allowance. An enclosing controlled producer may instead reserve `entry_bytes()` before ordinary insertion and retain that lease until the node is dropped. Separately allocated key/value payloads require their own reservations.
pub struct OwnedMap<K, V> {
    root: Link<K, V>,
    len: usize,
}

impl<K, V> Default for OwnedMap<K, V> {
    fn default() -> Self {
        Self { root: None, len: 0 }
    }
}

impl<K, V> OwnedMap<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Exact allocation layout of one node, including its key, value, links and padding. Allocator bookkeeping is outside the payload allowance.
    pub const fn entry_bytes() -> usize {
        size_of::<Node<K, V>>()
    }

    pub fn allocated_bytes(&self) -> usize {
        self.len * Self::entry_bytes()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    pub fn iter(&self) -> BudgetedMapIter<'_, K, V> {
        BudgetedMapIter::new(&self.root, self.len)
    }

    pub fn keys(&self) -> impl ExactSizeIterator<Item = &K> + std::iter::FusedIterator {
        self.iter().map(|(key, _)| key)
    }

    /// Mutate values without changing keys, node ownership or traversal storage.
    pub fn values_mut(
        &mut self,
    ) -> impl ExactSizeIterator<Item = &mut V> + std::iter::FusedIterator {
        iter_mut::IterMut::new(&mut self.root, self.len).map(|(_, value)| value)
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = &V> + std::iter::FusedIterator {
        self.iter().map(|(_, value)| value)
    }
}

impl<K: Ord, V> OwnedMap<K, V> {
    /// Borrow a value using a compound or differently encoded query without constructing an owned key. The comparator orders the query against each stored key and must agree with the key's `Ord` implementation.
    pub fn get_by(&self, compare: impl FnMut(&K) -> std::cmp::Ordering) -> Option<&V> {
        tree::get_by(&self.root, compare).map(|node| &node.value)
    }

    /// Mutate only a selected value, using the same query-to-key ordering as `get_by`. Keys and node ownership remain unchanged.
    pub fn get_mut_by(&mut self, compare: impl FnMut(&K) -> std::cmp::Ordering) -> Option<&mut V> {
        tree::get_mut_by(&mut self.root, compare).map(|node| &mut node.value)
    }

    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        tree::get(&self.root, key).map(|node| &node.value)
    }

    pub fn get_mut<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        tree::get_mut(&mut self.root, key).map(|node| &mut node.value)
    }

    pub fn contains_key<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get(key).is_some()
    }

    /// A collision replaces only the value, preserving the original key and node allocation.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if let Some(previous) = self.get_mut(&key) {
            return Some(std::mem::replace(previous, value));
        }
        let entry = OwnedNode {
            value: Box::new(Node {
                key,
                value,
                left: None,
                right: None,
                height: 1,
            }),
            memory: None,
        };
        let previous = tree::insert(&mut self.root, entry);
        self.len += 1;
        previous
    }

    /// Admit each newly allocated node before publication. Nodes retain their original leases independently, including when later inserts use another request's allowance; replacing a value does not replace its existing node or key. Key and value payloads keep their own owners.
    pub fn insert_budgeted(
        &mut self,
        key: K,
        value: V,
        budget: &MemoryBudget,
    ) -> Result<Option<V>, MemoryError> {
        self.insert_budgeted_with(key, budget, || Ok(value))
    }

    /// Admit a new node before constructing its value. A failed value producer
    /// releases the provisional node lease and preserves the previous map. A
    /// replacement needs no new node and is published only after production succeeds.
    pub fn insert_budgeted_with<E: From<MemoryError>>(
        &mut self,
        key: K,
        budget: &MemoryBudget,
        produce: impl FnOnce() -> Result<V, E>,
    ) -> Result<Option<V>, E> {
        if let Some(previous) = self.get_mut(&key) {
            return Ok(Some(std::mem::replace(previous, produce()?)));
        }
        let memory = budget.reserve(Self::entry_bytes())?;
        let value = produce()?;
        let entry = OwnedNode {
            value: Box::new(Node {
                key,
                value,
                left: None,
                right: None,
                height: 1,
            }),
            memory: Some(memory),
        };
        let previous = tree::insert(&mut self.root, entry);
        self.len += 1;
        Ok(previous)
    }

    /// Keep selected entries without constructing replacement nodes or releasing the retained entries' leases.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        let mut entries = std::mem::take(self).into_iter();
        while let Some(mut node) = entries.next_node() {
            if keep(&node.value.key, &mut node.value.value) {
                tree::insert(&mut self.root, node);
                self.len += 1;
            }
        }
    }

    /// Move entries from `other` without allocating replacement nodes. Collisions retain this map's original key and node, replacing only the value.
    pub fn append(&mut self, other: Self) {
        if self.is_empty() {
            *self = other;
            return;
        }
        let mut entries = other.into_iter();
        while let Some(node) = entries.next_node() {
            self.len += usize::from(tree::insert(&mut self.root, node).is_none());
        }
    }

    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        self.remove_entry(key).map(|(_, value)| value)
    }

    pub fn remove_entry<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
    {
        let removed = tree::remove(&mut self.root, key);
        self.len -= usize::from(removed.is_some());
        removed
    }

    /// Detach one entry while retaining its exact node, key, value and admission lease. Appending the returned map restores it without allocation.
    pub fn extract<Q: Ord + ?Sized>(&mut self, key: &Q) -> Self
    where
        K: Borrow<Q>,
    {
        let root = tree::extract(&mut self.root, key);
        let len = usize::from(root.is_some());
        self.len -= len;
        Self { root, len }
    }

    /// Borrow the first entry at or after a lower bound in logarithmic time without scratch allocation.
    pub fn first_from<Q: Ord + ?Sized>(&self, start: Bound<&Q>) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
    {
        let mut link = &self.root;
        let mut candidate = None;
        while let Some(node) = link {
            let included = match start {
                Bound::Unbounded => true,
                Bound::Included(key) => node.key.borrow() >= key,
                Bound::Excluded(key) => node.key.borrow() > key,
            };
            if included {
                candidate = Some((&node.key, &node.value.value));
                link = &node.left;
            } else {
                link = &node.right;
            }
        }
        candidate
    }
}

impl<K: Ord + Borrow<Q>, V, Q: Ord + ?Sized> std::ops::Index<&Q> for OwnedMap<K, V> {
    type Output = V;

    fn index(&self, key: &Q) -> &V {
        self.get(key).expect("missing owned map key")
    }
}

impl<K: Ord + Clone, V: Clone> Clone for OwnedMap<K, V> {
    fn clone(&self) -> Self {
        self.iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

impl<K: std::fmt::Debug, V: std::fmt::Debug> std::fmt::Debug for OwnedMap<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: PartialEq, V: PartialEq> PartialEq for OwnedMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<K: Eq, V: Eq> Eq for OwnedMap<K, V> {}

impl<K: Ord, V> FromIterator<(K, V)> for OwnedMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut map = Self::new();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl<K: Ord, V> Extend<(K, V)> for OwnedMap<K, V> {
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, entries: T) {
        for (key, value) in entries {
            self.insert(key, value);
        }
    }
}

impl<'a, K, V> IntoIterator for &'a OwnedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = BudgetedMapIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Consuming traversal keeps the pending AVL path on the stack and frees each visited node before yielding its entry.
pub struct OwnedMapIntoIter<K, V> {
    stack: [Option<OwnedNode<K, V>>; MAX_HEIGHT],
    depth: usize,
    remaining: usize,
}

impl<K, V> OwnedMapIntoIter<K, V> {
    fn push_left(&mut self, mut link: Link<K, V>) {
        while let Some(mut node) = link {
            link = node.value.left.take();
            self.stack[self.depth] = Some(node);
            self.depth += 1;
        }
    }

    fn next_node(&mut self) -> Option<OwnedNode<K, V>> {
        self.depth = self.depth.checked_sub(1)?;
        let mut node = self.stack[self.depth].take().expect("owned traversal node");
        self.push_left(node.value.right.take());
        node.value.height = 1;
        self.remaining -= 1;
        Some(node)
    }
}

impl<K, V> Iterator for OwnedMapIntoIter<K, V> {
    type Item = (K, V);

    fn next(&mut self) -> Option<Self::Item> {
        self.next_node().map(tree::into_entry)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<K, V> ExactSizeIterator for OwnedMapIntoIter<K, V> {}
impl<K, V> std::iter::FusedIterator for OwnedMapIntoIter<K, V> {}

impl<K, V> IntoIterator for OwnedMap<K, V> {
    type Item = (K, V);
    type IntoIter = OwnedMapIntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        let mut iter = OwnedMapIntoIter {
            stack: std::array::from_fn(|_| None),
            depth: 0,
            remaining: self.len,
        };
        iter.push_left(self.root);
        iter
    }
}

impl<K: crate::memory::ObserveAllocations, V: crate::memory::ObserveAllocations>
    crate::memory::ObserveAllocations for OwnedMap<K, V>
{
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut crate::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), crate::ValueRetentionError> {
        observer.child(&self.root)
    }
}
