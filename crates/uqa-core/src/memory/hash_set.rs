//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable hash maps and membership sets charge each complete bucket allocation to its owner.

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash},
};

use super::{BudgetedVec, MemoryBudget, MemoryError};

#[derive(Debug)]
struct Entry<K, V> {
    hash: u64,
    key: K,
    value: V,
}

/// A linear-probing map with reserved bucket layouts, including hashes, occupancy and alignment. Element-owned allocations remain the caller's responsibility. Growth retains both bucket buffers until every entry has moved; it never rehashes user values while publishing the replacement.
#[derive(Debug)]
pub struct BudgetedHashMap<K, V> {
    buckets: BudgetedVec<Option<Entry<K, V>>>,
    hasher: RandomState,
    len: usize,
}

impl<K, V> BudgetedHashMap<K, V> {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            buckets: BudgetedVec::new(budget),
            hasher: RandomState::new(),
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn reserve(&mut self, additional: usize) -> Result<(), MemoryError> {
        let required = self
            .len
            .checked_add(additional)
            .ok_or(MemoryError::SizeOverflow)?;
        // Keep at least half the buckets empty to limit clustering with linear probing.
        if required <= self.buckets.len() / 2 {
            return Ok(());
        }
        let capacity = required
            .checked_mul(2)
            .and_then(usize::checked_next_power_of_two)
            .ok_or(MemoryError::SizeOverflow)?;
        let mut replacement = BudgetedVec::new(self.buckets.budget());
        replacement.reserve(capacity)?;
        for _ in 0..capacity {
            replacement.push(None)?;
        }
        // Everything fallible, including allocation, finishes before the first move. Cached hashes avoid invoking application code during replacement.
        for bucket in self.buckets.iter_mut() {
            if let Some(entry) = bucket.take() {
                insert_vacant(&mut replacement, entry);
            }
        }
        self.buckets = replacement;
        Ok(())
    }
}

impl<K: Eq + Hash, V> BudgetedHashMap<K, V> {
    pub fn get(&self, key: &K) -> Option<&V> {
        let position = self.position(self.hasher.hash_one(key), key)?;
        Some(&self.buckets[position].as_ref()?.value)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let position = self.position(self.hasher.hash_one(key), key)?;
        Some(&mut self.buckets[position].as_mut()?.value)
    }

    pub fn insert(&mut self, key: K, value: V) -> Result<Option<V>, MemoryError> {
        let hash = self.hasher.hash_one(&key);
        if let Some(position) = self.position(hash, &key) {
            let entry = self.buckets[position].as_mut().expect("occupied position");
            return Ok(Some(std::mem::replace(&mut entry.value, value)));
        }
        self.reserve(1)?;
        insert_vacant(&mut self.buckets, Entry { hash, key, value });
        self.len += 1;
        Ok(None)
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let mut hole = self.position(self.hasher.hash_one(key), key)?;
        let removed = self.buckets[hole].take().expect("occupied position");
        self.len -= 1;
        let mask = self.buckets.len() - 1;
        let mut next = (hole + 1) & mask;
        while let Some(entry) = &self.buckets[next] {
            let origin = entry.hash as usize & mask;
            // Move an entry only when its probe from its origin to the next bucket crosses the hole.
            if (hole.wrapping_sub(origin) & mask) < (next.wrapping_sub(origin) & mask) {
                self.buckets[hole] = self.buckets[next].take();
                hole = next;
            }
            next = (next + 1) & mask;
        }
        Some(removed.value)
    }

    fn position(&self, hash: u64, key: &K) -> Option<usize> {
        if self.buckets.is_empty() {
            return None;
        }
        let mask = self.buckets.len() - 1;
        let mut position = hash as usize & mask;
        while let Some(entry) = &self.buckets[position] {
            if entry.hash == hash && entry.key == *key {
                return Some(position);
            }
            position = (position + 1) & mask;
        }
        None
    }
}

/// A membership set with the same budgeted buckets as the key/value map.
#[derive(Debug)]
pub struct BudgetedHashSet<T>(BudgetedHashMap<T, ()>);

impl<T> BudgetedHashSet<T> {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self(BudgetedHashMap::new(budget))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn reserve(&mut self, additional: usize) -> Result<(), MemoryError> {
        self.0.reserve(additional)
    }
}

impl<T: Eq + Hash> BudgetedHashSet<T> {
    pub fn contains(&self, value: &T) -> bool {
        self.0.get(value).is_some()
    }

    pub fn insert(&mut self, value: T) -> Result<bool, MemoryError> {
        self.0.insert(value, ()).map(|previous| previous.is_none())
    }

    pub fn remove(&mut self, value: &T) -> bool {
        self.0.remove(value).is_some()
    }
}

fn insert_vacant<K, V>(buckets: &mut [Option<Entry<K, V>>], entry: Entry<K, V>) {
    let mask = buckets.len() - 1;
    let mut position = entry.hash as usize & mask;
    while buckets[position].is_some() {
        position = (position + 1) & mask;
    }
    buckets[position] = Some(entry);
}

#[cfg(test)]
mod tests;
