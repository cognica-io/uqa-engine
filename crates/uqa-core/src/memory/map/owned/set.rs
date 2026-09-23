//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sets reuse exact owned map nodes without a second tree implementation.

use std::borrow::Borrow;

use super::{BudgetedMapIter, OwnedMap, OwnedMapIntoIter};

/// An ordinary ordered set with exact node layouts and the same enclosing-owner admission contract as `OwnedMap`.
#[derive(Debug, PartialEq, Eq)]
pub struct OwnedSet<K>(OwnedMap<K, ()>);

impl<K: Ord + Clone> Clone for OwnedSet<K> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<K> Default for OwnedSet<K> {
    fn default() -> Self {
        Self(OwnedMap::new())
    }
}

impl<K> OwnedSet<K> {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn entry_bytes() -> usize {
        OwnedMap::<K, ()>::entry_bytes()
    }

    pub fn allocated_bytes(&self) -> usize {
        self.0.allocated_bytes()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> OwnedSetIter<'_, K> {
        OwnedSetIter(self.0.iter())
    }
}

impl<K: Ord> OwnedSet<K> {
    pub fn insert(&mut self, key: K) -> bool {
        self.0.insert(key, ()).is_none()
    }

    pub fn contains<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.0.contains_key(key)
    }
}

impl<K: Ord> FromIterator<K> for OwnedSet<K> {
    fn from_iter<T: IntoIterator<Item = K>>(iter: T) -> Self {
        Self(iter.into_iter().map(|key| (key, ())).collect())
    }
}

pub struct OwnedSetIter<'a, K>(BudgetedMapIter<'a, K, ()>);

impl<'a, K> Iterator for OwnedSetIter<'a, K> {
    type Item = &'a K;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(key, ())| key)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<K> ExactSizeIterator for OwnedSetIter<'_, K> {}
impl<K> std::iter::FusedIterator for OwnedSetIter<'_, K> {}

impl<'a, K> IntoIterator for &'a OwnedSet<K> {
    type Item = &'a K;
    type IntoIter = OwnedSetIter<'a, K>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct OwnedSetIntoIter<K>(OwnedMapIntoIter<K, ()>);

impl<K> Iterator for OwnedSetIntoIter<K> {
    type Item = K;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(key, ())| key)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<K> ExactSizeIterator for OwnedSetIntoIter<K> {}
impl<K> std::iter::FusedIterator for OwnedSetIntoIter<K> {}

impl<K> IntoIterator for OwnedSet<K> {
    type Item = K;
    type IntoIter = OwnedSetIntoIter<K>;

    fn into_iter(self) -> Self::IntoIter {
        OwnedSetIntoIter(self.0.into_iter())
    }
}
