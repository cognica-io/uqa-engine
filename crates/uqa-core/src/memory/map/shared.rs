//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable ordered roots share entries and unchanged subtrees under one allowance.

#[cfg(test)]
mod tests;
mod tree;

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

/// An immutable ordered map with logarithmic lookup and insertion. Cloning shares a root without copying entries or allocating. Updates reserve copied search paths before allocation; unchanged entries and subtrees keep their original reservations until their last root is dropped. Keys and values retain their separately owned payloads.
pub struct BudgetedSharedMap<K, V> {
    root: Link<K, V>,
    len: usize,
    memory: MemoryBudget,
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

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn budget(&self) -> &MemoryBudget {
        &self.memory
    }

    pub fn iter(&self) -> BudgetedSharedMapIter<'_, K, V> {
        let mut iter = BudgetedSharedMapIter::empty();
        iter.push_left(self.root.as_ref().map(|node| &***node));
        iter
    }
}

impl<K: Ord, V> BudgetedSharedMap<K, V> {
    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        let mut link = &self.root;
        while let Some(node) = link {
            match key.cmp(node.entry.0.borrow()) {
                Ordering::Less => link = &node.left,
                Ordering::Greater => link = &node.right,
                Ordering::Equal => return Some(&node.entry.1),
            }
        }
        None
    }

    /// Return a new root containing the supplied key and value, leaving this root unchanged even if reservation fails. Matching keys are replaced together with their values; neither type needs to implement `Clone`.
    pub fn with_insert(&self, key: K, value: V) -> Result<Self, MemoryError> {
        let entry = Budgeted::new((key, value), self.memory.empty_reservation()).into_shared()?;
        let (root, added) = tree::insert(&self.root, entry, &self.memory)?;
        Ok(Self {
            root: Some(root),
            len: self.len + usize::from(added),
            memory: self.memory.clone(),
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
