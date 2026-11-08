//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable ordered roots share entries and unchanged subtrees under one allowance.

mod bidirectional;
#[cfg(test)]
mod tests;
mod tree;

pub use bidirectional::BudgetedSharedMapBidirectionalIter;

use std::{borrow::Borrow, cmp::Ordering, ops::Bound, sync::Arc};

use super::{Budgeted, MemoryBudget, MemoryError, MAX_HEIGHT};

type Entry<K, V> = Arc<Budgeted<(K, V)>>;
type SharedNode<K, V> = Arc<Budgeted<Node<K, V>>>;
type Link<K, V> = Option<SharedNode<K, V>>;

struct Node<K, V> {
    entry: Entry<K, V>,
    left: Link<K, V>,
    right: Link<K, V>,
    height: u8,
}

/// A persistent ordered map with logarithmic lookup, insertion and deletion. Cloning shares a root without copying entries or allocating. Updates reserve their complete path construction before mutation. Insertion reuses unique nodes; immutable deletion copies the affected paths and rotation pivots. Unchanged entries and subtrees keep their original reservations until their last root is dropped. Keys and values retain their separately owned payloads.
pub struct BudgetedSharedMap<K, V> {
    root: Link<K, V>,
    len: usize,
    memory: MemoryBudget,
}

/// An exclusively borrowed insertion with its key comparisons and structural allowance already complete. Dropping it leaves the original root unchanged; publication supplies the separately owned value without another comparison or admission.
pub struct PreparedSharedMapInsert<'a, K, V> {
    map: &'a mut BudgetedSharedMap<K, V>,
    insertion: tree::PreparedInsert<K, V>,
}

impl<K, V> PreparedSharedMapInsert<'_, K, V> {
    pub fn publish(self, value: V) {
        let added = self.insertion.publish(&mut self.map.root, value);
        self.map.len += usize::from(added);
    }
}

/// An immutable ordered root and its cardinality. Its nodes retain their original allocation leases; an empty root owns no allocations. Cloning does not copy entries or allocate, and restoring into a mutable map requires its original allowance for a nonempty root.
pub struct BudgetedSharedMapSnapshot<K, V> {
    root: Link<K, V>,
    len: usize,
}

impl<K, V> Clone for BudgetedSharedMapSnapshot<K, V> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            len: self.len,
        }
    }
}

impl<K, V> BudgetedSharedMapSnapshot<K, V> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Requested node and entry layouts reachable from this root, excluding separately owned key/value payloads and reference-count bookkeeping. Each entry occurs once within one root, even when other roots share it.
    pub fn structural_bytes(&self) -> Result<usize, MemoryError> {
        self.len
            .checked_mul(size_of::<Budgeted<Node<K, V>>>() + size_of::<Budgeted<(K, V)>>())
            .ok_or(MemoryError::SizeOverflow)
    }

    /// Borrow entries in key order without allocating or retaining a mutable map.
    pub fn iter(&self) -> BudgetedSharedMapIter<'_, K, V> {
        let mut iter = BudgetedSharedMapIter::empty();
        iter.push_left(self.root.as_ref().map(|node| &***node));
        iter
    }
}

impl<K: Ord, V> BudgetedSharedMapSnapshot<K, V> {
    /// Select the least entry at a lower bound without allocating or retaining a mutable map.
    pub fn first_from<Q: Ord + ?Sized>(&self, start: Bound<&Q>) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
    {
        first_from(&self.root, start)
    }

    /// Borrow both ends in constant space, seeking each next key in logarithmic time.
    pub fn iter_bidirectional(&self) -> BudgetedSharedMapBidirectionalIter<'_, K, V> {
        BudgetedSharedMapBidirectionalIter::new(&self.root, self.len)
    }

    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        lookup(&self.root, key)
    }
}

fn first_from<'a, K: Borrow<Q>, V, Q: Ord + ?Sized>(
    root: &'a Link<K, V>,
    start: Bound<&Q>,
) -> Option<(&'a K, &'a V)> {
    let mut link = root;
    let mut found = None;
    while let Some(node) = link {
        let included = match start {
            Bound::Unbounded => true,
            Bound::Included(key) => node.entry.0.borrow() >= key,
            Bound::Excluded(key) => node.entry.0.borrow() > key,
        };
        if included {
            found = Some((&node.entry.0, &node.entry.1));
            link = &node.left;
        } else {
            link = &node.right;
        }
    }
    found
}

impl<K, V> Clone for BudgetedSharedMap<K, V> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            len: self.len,
            memory: self.memory.clone(),
        }
    }
}

impl<K, V> BudgetedSharedMap<K, V> {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            root: None,
            len: 0,
            memory: memory.clone(),
        }
    }

    /// True when both owners retain the same immutable entry graph. This observes identity without visiting entries or comparing keys and values.
    pub fn shares_root(&self, other: &Self) -> bool {
        match (&self.root, &other.root) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn budget(&self) -> &MemoryBudget {
        &self.memory
    }

    /// Select the allowance for later path construction without changing existing node owners. `RetainedMap` exposes this transfer with read-only snapshots; the raw map's same-allowance restore API does not expose transferred roots.
    pub(in crate::memory) fn into_mutation_budget(mut self, memory: &MemoryBudget) -> Self {
        self.memory = memory.clone();
        self
    }

    /// Retain this immutable root without another mutable allowance handle or any allocation.
    pub fn snapshot(&self) -> BudgetedSharedMapSnapshot<K, V> {
        BudgetedSharedMapSnapshot {
            root: self.root.clone(),
            len: self.len,
        }
    }

    /// Restore a retained root without allocating. Panics before mutation if a nonempty root belongs to another allowance; empty roots own no allocations and can be restored into any allowance.
    pub fn restore(&mut self, snapshot: &BudgetedSharedMapSnapshot<K, V>) {
        if let Some(root) = &snapshot.root {
            assert!(
                self.memory.shares_allowance(root.budget()),
                "different memory allowances"
            );
        }
        self.root.clone_from(&snapshot.root);
        self.len = snapshot.len;
    }

    pub fn iter(&self) -> BudgetedSharedMapIter<'_, K, V> {
        let mut iter = BudgetedSharedMapIter::empty();
        iter.push_left(self.root.as_ref().map(|node| &***node));
        iter
    }
}

impl<K: Ord, V> BudgetedSharedMap<K, V> {
    /// Select the least entry at a lower bound without allocating a traversal cursor.
    pub fn first_from<Q: Ord + ?Sized>(&self, start: Bound<&Q>) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
    {
        first_from(&self.root, start)
    }

    /// Borrow both ends in constant space, seeking each next key in logarithmic time.
    pub fn iter_bidirectional(&self) -> BudgetedSharedMapBidirectionalIter<'_, K, V> {
        BudgetedSharedMapBidirectionalIter::new(&self.root, self.len)
    }

    /// Select the greatest entry at an upper bound in logarithmic time, without allocating or traversing preceding entries.
    pub fn last_before<Q: Ord + ?Sized>(&self, end: Bound<&Q>) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
    {
        let mut link = &self.root;
        let mut found = None;
        while let Some(node) = link {
            let included = match end {
                Bound::Unbounded => true,
                Bound::Included(key) => node.entry.0.borrow() <= key,
                Bound::Excluded(key) => node.entry.0.borrow() < key,
            };
            if included {
                found = Some((&node.entry.0, &node.entry.1));
                link = &node.right;
            } else {
                link = &node.left;
            }
        }
        found
    }

    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        lookup(&self.root, key)
    }

    /// Return a new root containing the supplied key and value, leaving this root unchanged even if reservation fails. Matching keys are replaced together with their values; neither type needs to implement `Clone`.
    pub fn with_insert(&self, key: K, value: V) -> Result<Self, MemoryError> {
        let mut candidate = self.clone();
        candidate.try_insert(key, value)?;
        Ok(candidate)
    }

    /// Return an immutable successor without the selected key. Unchanged entries and subtrees retain their original leases. Missing keys allocate nothing; removal reserves a logarithmic path and its rotations before constructing the successor, leaving this root unchanged on failure.
    pub fn with_remove<Q: Ord + ?Sized>(&self, key: &Q) -> Result<Self, MemoryError>
    where
        K: Borrow<Q>,
    {
        let (root, removed) = tree::remove(&self.root, key, &self.memory)?;
        Ok(Self {
            root,
            len: self.len - usize::from(removed),
            memory: self.memory.clone(),
        })
    }

    /// Reserve the complete insertion before changing this root, reusing unique nodes and copying only shared paths. Reservation failure preserves this root; existing clones remain unchanged. Matching keys and values are replaced together without requiring `Clone`.
    pub fn try_insert(&mut self, key: K, value: V) -> Result<(), MemoryError> {
        self.prepare_insert(key)?.publish(value);
        Ok(())
    }

    /// Admit the complete insertion before an owning operation obtains its final value. The exclusive borrow fixes the selected path until publication or abandonment; separately allocated key/value payloads retain their own admission.
    pub fn prepare_insert(
        &mut self,
        key: K,
    ) -> Result<PreparedSharedMapInsert<'_, K, V>, MemoryError> {
        let insertion = tree::prepare(&self.root, key, &self.memory)?;
        Ok(PreparedSharedMapInsert {
            map: self,
            insertion,
        })
    }

    /// Visit keys in order, seeking the lower bound without traversing earlier entries or allocating a traversal buffer.
    pub fn range_from<Q: Ord + ?Sized>(&self, start: Bound<&Q>) -> BudgetedSharedMapIter<'_, K, V>
    where
        K: Borrow<Q>,
    {
        let mut iter = BudgetedSharedMapIter::empty();
        let mut link = &self.root;
        while let Some(node) = link {
            let included = match start {
                Bound::Unbounded => true,
                Bound::Included(key) => node.entry.0.borrow() >= key,
                Bound::Excluded(key) => node.entry.0.borrow() > key,
            };
            if included {
                iter.stack[iter.depth] = Some(node);
                iter.depth += 1;
                link = &node.left;
            } else {
                link = &node.right;
            }
        }
        iter
    }
}

fn lookup<'a, K: Borrow<Q>, V, Q: Ord + ?Sized>(
    mut link: &'a Link<K, V>,
    key: &Q,
) -> Option<&'a V> {
    while let Some(node) = link {
        match key.cmp(node.entry.0.borrow()) {
            Ordering::Less => link = &node.left,
            Ordering::Greater => link = &node.right,
            Ordering::Equal => return Some(&node.entry.1),
        }
    }
    None
}

pub struct BudgetedSharedMapIter<'a, K, V> {
    stack: [Option<&'a Node<K, V>>; MAX_HEIGHT],
    depth: usize,
}

impl<'a, K, V> BudgetedSharedMapIter<'a, K, V> {
    fn empty() -> Self {
        Self {
            stack: [None; MAX_HEIGHT],
            depth: 0,
        }
    }

    fn push_left(&mut self, mut node: Option<&'a Node<K, V>>) {
        while let Some(current) = node {
            self.stack[self.depth] = Some(current);
            self.depth += 1;
            node = current.left.as_ref().map(|node| &***node);
        }
    }
}

impl<'a, K, V> Iterator for BudgetedSharedMapIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.depth = self.depth.checked_sub(1)?;
        let node = self.stack[self.depth].take().expect("retained map node");
        self.push_left(node.right.as_ref().map(|node| &***node));
        Some((&node.entry.0, &node.entry.1))
    }
}

impl<K, V> std::iter::FusedIterator for BudgetedSharedMapIter<'_, K, V> {}

impl<'a, K, V> IntoIterator for &'a BudgetedSharedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = BudgetedSharedMapIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K, V> IntoIterator for &'a BudgetedSharedMapSnapshot<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = BudgetedSharedMapIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
