//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered maps reserve complete AVL nodes before publication, including links and padding.

#[cfg(test)]
mod tests;
mod tree;

use std::borrow::Borrow;

use super::{Budgeted, MemoryBudget, MemoryError};

type OwnedNode<K, V> = Budgeted<Box<Node<K, V>>>;
type Link<K, V> = Option<OwnedNode<K, V>>;

struct Node<K, V> {
    key: K,
    value: V,
    left: Link<K, V>,
    right: Link<K, V>,
    height: u8,
}

/// An ordered map with logarithmic lookup, insertion and removal. Each node reserves its entire allocation before construction; keys and values retain their own separately allocated payloads. Removing a node frees its allocation before releasing that reservation. The map never clones elements or retains unused nodes.
pub struct BudgetedMap<K, V> {
    root: Link<K, V>,
    len: usize,
    memory: MemoryBudget,
}

/// An unpublished node and its reservation. Preparing several entries allows an owner to finish every fallible reservation before publishing any map mutation.
pub struct PreparedMapEntry<K, V> {
    node: OwnedNode<K, V>,
}

impl<K, V> BudgetedMap<K, V> {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            root: None,
            len: 0,
            memory: memory.clone(),
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

    pub fn prepare_entry(&self, key: K, value: V) -> Result<PreparedMapEntry<K, V>, MemoryError> {
        let memory = self.memory.reserve(size_of::<Node<K, V>>())?;
        let node = Box::new(Node {
            key,
            value,
            left: None,
            right: None,
            height: 1,
        });
        Ok(PreparedMapEntry {
            node: Budgeted::new(node, memory),
        })
    }

    /// Visit mutable values in key order without allocating a traversal buffer or allowing keys to change.
    pub fn for_each_mut(&mut self, mut visit: impl FnMut(&K, &mut V)) {
        tree::for_each_mut(&mut self.root, &mut visit);
    }

    pub fn iter(&self) -> BudgetedMapIter<'_, K, V> {
        let mut iter = BudgetedMapIter {
            stack: [None; MAX_HEIGHT],
            depth: 0,
            remaining: self.len,
        };
        iter.push_left(self.root.as_ref().map(|node| &***node));
        iter
    }
}

impl<K: Ord, V> BudgetedMap<K, V> {
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

    /// Publish an already reserved entry without allocation. A matching key retains its original key and returns the replaced value. Panics before mutation if the entry belongs to a different allowance.
    pub fn insert_prepared(&mut self, entry: PreparedMapEntry<K, V>) -> Option<V> {
        assert!(
            self.memory.shares_allowance(entry.node.memory.budget()),
            "different memory allowances"
        );
        let previous = tree::insert(&mut self.root, entry.node);
        self.len += usize::from(previous.is_none());
        previous
    }

    /// Reserve a node only for a new key. Failure preserves the map, its entries and its reservations.
    pub fn insert(&mut self, key: K, value: V) -> Result<Option<V>, MemoryError> {
        if let Some(previous) = self.get_mut(&key) {
            return Ok(Some(std::mem::replace(previous, value)));
        }
        let entry = self.prepare_entry(key, value)?;
        Ok(self.insert_prepared(entry))
    }

    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        let removed = tree::remove(&mut self.root, key);
        self.len -= usize::from(removed.is_some());
        removed.map(|(_, value)| value)
    }
}

impl<K: Ord + Borrow<Q>, V, Q: Ord + ?Sized> std::ops::Index<&Q> for BudgetedMap<K, V> {
    type Output = V;

    fn index(&self, key: &Q) -> &V {
        self.get(key).expect("missing budgeted map key")
    }
}

// Every two AVL levels at least double the minimum node count. Even zero-sized keys and values need nonzero links, so this exceeds every addressable tree's height.
const MAX_HEIGHT: usize = usize::BITS as usize * 2;

pub struct BudgetedMapIter<'a, K, V> {
    stack: [Option<&'a Node<K, V>>; MAX_HEIGHT],
    depth: usize,
    remaining: usize,
}

impl<'a, K, V> BudgetedMapIter<'a, K, V> {
    fn push_left(&mut self, mut node: Option<&'a Node<K, V>>) {
        while let Some(current) = node {
            self.stack[self.depth] = Some(current);
            self.depth += 1;
            node = current.left.as_ref().map(|node| &***node);
        }
    }
}

impl<'a, K, V> Iterator for BudgetedMapIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.depth = self.depth.checked_sub(1)?;
        let node = self.stack[self.depth].take().expect("retained map node");
        self.push_left(node.right.as_ref().map(|node| &***node));
        self.remaining -= 1;
        Some((&node.key, &node.value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<K, V> ExactSizeIterator for BudgetedMapIter<'_, K, V> {}
impl<K, V> std::iter::FusedIterator for BudgetedMapIter<'_, K, V> {}

impl<'a, K, V> IntoIterator for &'a BudgetedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = BudgetedMapIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
