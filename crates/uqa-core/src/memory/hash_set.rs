//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Insert-only membership sets retain the complete bucket allocation under one allowance.

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash},
};

use super::{BudgetedVec, MemoryBudget, MemoryError};

#[derive(Debug)]
struct Entry<T> {
    hash: u64,
    value: T,
}

/// A linear-probing set with reserved bucket layouts, including hashes, occupancy and alignment. Element-owned allocations remain the caller's responsibility. Growth retains both bucket buffers until every entry has moved; it never rehashes user values while publishing the replacement.
#[derive(Debug)]
pub struct BudgetedHashSet<T> {
    buckets: BudgetedVec<Option<Entry<T>>>,
    hasher: RandomState,
    len: usize,
}

impl<T> BudgetedHashSet<T> {
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

impl<T: Eq + Hash> BudgetedHashSet<T> {
    pub fn contains(&self, value: &T) -> bool {
        self.contains_hashed(self.hasher.hash_one(value), value)
    }

    pub fn insert(&mut self, value: T) -> Result<bool, MemoryError> {
        let hash = self.hasher.hash_one(&value);
        if self.contains_hashed(hash, &value) {
            return Ok(false);
        }
        self.reserve(1)?;
        insert_vacant(&mut self.buckets, Entry { hash, value });
        self.len += 1;
        Ok(true)
    }

    fn contains_hashed(&self, hash: u64, value: &T) -> bool {
        if self.buckets.is_empty() {
            return false;
        }
        let mask = self.buckets.len() - 1;
        let mut position = hash as usize & mask;
        while let Some(entry) = &self.buckets[position] {
            if entry.hash == hash && entry.value == *value {
                return true;
            }
            position = (position + 1) & mask;
        }
        false
    }
}

fn insert_vacant<T>(buckets: &mut [Option<Entry<T>>], entry: Entry<T>) {
    let mask = buckets.len() - 1;
    let mut position = entry.hash as usize & mask;
    while buckets[position].is_some() {
        position = (position + 1) & mask;
    }
    buckets[position] = Some(entry);
}

#[cfg(test)]
mod tests;
