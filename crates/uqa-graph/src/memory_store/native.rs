//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph-owned payload leases accompany Core's immutable ordered roots.

use std::{borrow::Borrow, collections::BTreeSet, fmt, ops::Bound};
use uqa_core::memory::{
    MemoryBudget, MemoryError, MemoryReservation, RetainedMap, RetainedMapIter,
};

struct Entry<V> {
    value: V,
    _memory: MemoryReservation,
}

pub(super) struct NativeMap<K, V>(RetainedMap<K, Entry<V>>);

impl<K, V> Clone for NativeMap<K, V> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<K, V> NativeMap<K, V> {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self(RetainedMap::new(memory))
    }

    pub(super) fn budget(&self) -> &MemoryBudget {
        self.0.budget()
    }

    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.0.clear();
    }

    pub(super) fn retained_bytes(&self) -> Result<usize, MemoryError> {
        self.0.retained_bytes()
    }

    pub(super) fn iter(&self) -> NativeMapIter<'_, K, V> {
        NativeMapIter(self.0.iter())
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(key, _)| key)
    }
}

impl<K: Ord, V> NativeMap<K, V> {
    pub(super) fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.0.get(key).map(|entry| &entry.value)
    }

    pub(super) fn contains_key<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get(key).is_some()
    }

    pub(super) fn last_key_value(&self) -> Option<(&K, &V)> {
        self.0
            .last_before::<K>(Bound::Unbounded)
            .map(|(key, entry)| (key, &entry.value))
    }

    pub(super) fn range_from<Q: Ord + ?Sized>(&self, start: Bound<&Q>) -> NativeMapIter<'_, K, V>
    where
        K: Borrow<Q>,
    {
        NativeMapIter(self.0.range_from(start))
    }

    /// Direct allocations receive a lease here; nested native roots already own theirs. Capture includes both through the separately supplied complete payload footprint.
    pub(super) fn insert(
        &mut self,
        key: K,
        value: V,
        own_bytes: usize,
        retained_bytes: usize,
    ) -> Result<(), MemoryError> {
        let memory = self.budget().reserve(own_bytes)?;
        self.insert_admitted(key, value, memory, retained_bytes)
    }

    pub(super) fn insert_admitted(
        &mut self,
        key: K,
        value: V,
        memory: MemoryReservation,
        retained_bytes: usize,
    ) -> Result<(), MemoryError> {
        self.0.insert(
            key,
            Entry {
                value,
                _memory: memory,
            },
            retained_bytes,
        )
    }

    pub(super) fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Result<bool, MemoryError>
    where
        K: Borrow<Q>,
    {
        self.0.remove(key)
    }
}

impl<V> NativeMap<String, V> {
    pub(super) fn insert_named(
        &mut self,
        name: &str,
        value: V,
        own_payload: usize,
        retained_payload: usize,
    ) -> Result<(), MemoryError> {
        let mut memory = self.budget().reserve(own_payload)?;
        let mut key = uqa_core::memory::BudgetedString::new(self.budget());
        key.push_str(name)?;
        let (key, key_memory) = key.into_parts();
        let retained = key
            .capacity()
            .checked_add(retained_payload)
            .ok_or(MemoryError::SizeOverflow)?;
        memory.absorb(key_memory);
        self.insert_admitted(key, value, memory, retained)
    }
}

pub(super) struct NativeMapIter<'a, K, V>(RetainedMapIter<'a, K, Entry<V>>);

impl<'a, K, V> Iterator for NativeMapIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(key, entry)| (key, &entry.value))
    }
}

impl<'a, K, V> IntoIterator for &'a NativeMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = NativeMapIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for NativeMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

#[derive(Debug)]
pub(super) struct NativeSet<K>(NativeMap<K, ()>);

impl<K> Clone for NativeSet<K> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<K> NativeSet<K> {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self(NativeMap::new(memory))
    }
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(super) fn retained_bytes(&self) -> Result<usize, MemoryError> {
        self.0.retained_bytes()
    }
    pub(super) fn iter(&self) -> NativeSetIter<'_, K> {
        NativeSetIter(self.0.iter())
    }
}

impl<K: Ord> NativeSet<K> {
    pub(super) fn contains<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.0.contains_key(key)
    }
    pub(super) fn last(&self) -> Option<&K> {
        self.0.last_key_value().map(|(key, ())| key)
    }
    pub(super) fn range_from<Q: Ord + ?Sized>(&self, start: Bound<&Q>) -> impl Iterator<Item = &K>
    where
        K: Borrow<Q>,
    {
        self.0.range_from(start).map(|(key, ())| key)
    }
    pub(super) fn insert(&mut self, key: K, bytes: usize) -> Result<(), MemoryError> {
        self.0.insert(key, (), bytes, bytes)
    }
    pub(super) fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Result<bool, MemoryError>
    where
        K: Borrow<Q>,
    {
        self.0.remove(key)
    }
}

impl<K: Ord + Clone> NativeSet<K> {
    pub(super) fn to_set(&self) -> BTreeSet<K> {
        self.iter().cloned().collect()
    }
}

pub(super) struct NativeSetIter<'a, K>(NativeMapIter<'a, K, ()>);

impl<'a, K> Iterator for NativeSetIter<'a, K> {
    type Item = &'a K;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(key, ())| key)
    }
}

impl<'a, K> IntoIterator for &'a NativeSet<K> {
    type Item = &'a K;
    type IntoIter = NativeSetIter<'a, K>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl NativeSet<String> {
    pub(super) fn insert_name(&mut self, name: &str) -> Result<(), MemoryError> {
        if self.contains(name) {
            return Ok(());
        }
        self.0.insert_named(name, (), 0, 0)
    }
}
