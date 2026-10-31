//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable hash indexes admit bucket layouts and retain each key and value with its original lease.

use super::{Budgeted, BudgetedVec, MemoryBudget, MemoryError};
use std::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
};

mod iter;
pub use iter::BudgetedHashIndexIter;

#[derive(Debug)]
struct Entry<K, V> {
    hash: u64,
    key: Budgeted<K>,
    value: Budgeted<V>,
}

/// A completed lookup index. Construction owns its unpublished buckets, so cancellation during growth drops both generations and every moved payload; no partially rewritten index can escape.
#[derive(Debug)]
pub struct BudgetedHashIndex<K, V, H> {
    buckets: BudgetedVec<Option<Entry<K, V>>>,
    hasher: H,
    len: usize,
}

impl<K: Eq + Hash, V, H: BuildHasher> BudgetedHashIndex<K, V, H> {
    pub fn build<E: From<MemoryError>>(
        budget: &MemoryBudget,
        hasher: H,
        entries: impl IntoIterator<Item = Result<(Budgeted<K>, Budgeted<V>), E>>,
        mut check: impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        check()?;
        let mut output = Self {
            buckets: BudgetedVec::new(budget),
            hasher,
            len: 0,
        };
        let mut entries = entries.into_iter();
        loop {
            check()?;
            let Some(entry) = entries.next() else { break };
            let (key, value) = entry?;
            assert!(
                budget.shares_allowance(key.memory.budget()),
                "different hash-key allowance"
            );
            assert!(
                budget.shares_allowance(value.memory.budget()),
                "different hash-value allowance"
            );
            let hash = output.hasher.hash_one(&*key);
            if let Some(position) = output.position_with_check(hash, &*key, &mut check)? {
                output.buckets[position]
                    .as_mut()
                    .expect("selected entry")
                    .value = value;
                continue;
            }
            output.reserve_next(&mut check)?;
            let position = vacant(&output.buckets, hash, &mut check)?;
            output.buckets[position] = Some(Entry { hash, key, value });
            output.len += 1;
        }
        check()?;
        Ok(output)
    }

    fn reserve_next<E: From<MemoryError>>(
        &mut self,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), E> {
        let required = self.len.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
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
            check()?;
            replacement.push(None)?;
        }
        for bucket in self.buckets.iter_mut() {
            check()?;
            if let Some(entry) = bucket.take() {
                let position = vacant(&replacement, entry.hash, check)?;
                replacement[position] = Some(entry);
            }
        }
        self.buckets = replacement;
        Ok(())
    }

    fn position_with_check<Q: Eq + ?Sized, E>(
        &self,
        hash: u64,
        key: &Q,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Option<usize>, E>
    where
        K: Borrow<Q>,
    {
        if self.buckets.is_empty() {
            return Ok(None);
        }
        let mask = self.buckets.len() - 1;
        let mut position = hash as usize & mask;
        while let Some(entry) = &self.buckets[position] {
            check()?;
            if entry.hash == hash && (*entry.key).borrow() == key {
                return Ok(Some(position));
            }
            position = (position + 1) & mask;
        }
        Ok(None)
    }

    pub fn get<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        let position = self
            .position_with_check(self.hasher.hash_one(key), key, &mut || {
                Ok::<_, std::convert::Infallible>(())
            })
            .expect("ordinary index lookup cannot fail")?;
        self.buckets[position].as_ref().map(|entry| &*entry.value)
    }

    pub fn get_with_check<Q: Eq + Hash + ?Sized, E>(
        &self,
        key: &Q,
        mut check: impl FnMut() -> Result<(), E>,
    ) -> Result<Option<&V>, E>
    where
        K: Borrow<Q>,
    {
        check()?;
        let position = self.position_with_check(self.hasher.hash_one(key), key, &mut check)?;
        Ok(
            position
                .and_then(|position| self.buckets[position].as_ref().map(|entry| &*entry.value)),
        )
    }
}

impl<K, V, H> BudgetedHashIndex<K, V, H> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn bucket_bytes(&self) -> usize {
        self.buckets.capacity() * size_of::<Option<Entry<K, V>>>()
    }
    pub fn iter(&self) -> BudgetedHashIndexIter<'_, K, V> {
        BudgetedHashIndexIter {
            buckets: self.buckets.iter(),
            remaining: self.len,
        }
    }
}

impl<'a, K, V, H> IntoIterator for &'a BudgetedHashIndex<K, V, H> {
    type Item = (&'a K, &'a V);
    type IntoIter = BudgetedHashIndexIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

fn vacant<K, V, E>(
    buckets: &[Option<Entry<K, V>>],
    hash: u64,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<usize, E> {
    let mask = buckets.len() - 1;
    let mut position = hash as usize & mask;
    while buckets[position].is_some() {
        check()?;
        position = (position + 1) & mask;
    }
    Ok(position)
}

#[cfg(test)]
mod tests;
