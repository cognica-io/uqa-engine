//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sets reuse exact owned map nodes without a second tree implementation.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
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

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn iter(&self) -> OwnedSetIter<'_, K> {
        OwnedSetIter(self.0.iter())
    }
}

impl<K: Ord> OwnedSet<K> {
    pub fn insert(&mut self, key: K) -> bool {
        self.0.insert(key, ()).is_none()
    }

    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.0.remove(key).is_some()
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

impl<K: Ord> Extend<K> for OwnedSet<K> {
    fn extend<T: IntoIterator<Item = K>>(&mut self, iter: T) {
        for key in iter {
            self.insert(key);
        }
    }
}

impl<K: Ord, const N: usize> From<[K; N]> for OwnedSet<K> {
    fn from(keys: [K; N]) -> Self {
        keys.into_iter().collect()
    }
}

impl<K: Serialize> Serialize for OwnedSet<K> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de, K: Ord + Deserialize<'de>> Deserialize<'de> for OwnedSet<K> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<K>(std::marker::PhantomData<K>);
        impl<'de, K: Ord + Deserialize<'de>> serde::de::Visitor<'de> for Visitor<K> {
            type Value = OwnedSet<K>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an ordered set")
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut result = OwnedSet::new();
                while let Some(key) = sequence.next_element()? {
                    result.insert(key);
                }
                Ok(result)
            }
        }
        deserializer.deserialize_seq(Visitor(std::marker::PhantomData))
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
