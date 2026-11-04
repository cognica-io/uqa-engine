//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable map generations with owner-supplied payload footprints and independent capture admission.

use super::{
    Budgeted, BudgetedSharedMap, BudgetedSharedMapIter, BudgetedSharedMapSnapshot, MemoryBudget,
    MemoryError,
};
use std::borrow::Borrow;

struct Entry<V> {
    value: V,
    payload_bytes: usize,
}

/// The owner supplies separately allocated key/value capacities at insertion and must keep those payloads immutable. The map maintains their sum with each successful publication; its construction allowance covers tree nodes, while each capture independently admits nodes and payloads.
pub struct RetainedMap<K, V> {
    values: BudgetedSharedMap<K, Entry<V>>,
    payload_bytes: usize,
}

impl<K, V> Clone for RetainedMap<K, V> {
    fn clone(&self) -> Self {
        Self {
            values: self.values.clone(),
            payload_bytes: self.payload_bytes,
        }
    }
}

impl<K, V> RetainedMap<K, V> {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            values: BudgetedSharedMap::new(memory),
            payload_bytes: 0,
        }
    }

    pub fn budget(&self) -> &MemoryBudget {
        self.values.budget()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> RetainedMapIter<'_, K, V> {
        RetainedMapIter(self.values.iter())
    }

    pub fn clear(&mut self) {
        self.values = BudgetedSharedMap::new(self.values.budget());
        self.payload_bytes = 0;
    }

    /// Capture takes constant work, allocates no tree nodes, and does not enumerate payloads. The returned lease must outlive its immutable root.
    pub fn capture(
        &self,
        memory: &MemoryBudget,
    ) -> Result<Budgeted<RetainedMapSnapshot<K, V>>, MemoryError> {
        let values = self.values.snapshot();
        let bytes = values
            .structural_bytes()?
            .checked_add(self.payload_bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(Budgeted::new(
            RetainedMapSnapshot { values },
            memory.reserve(bytes)?,
        ))
    }
}

impl<K: Ord, V> RetainedMap<K, V> {
    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.values.get(key).map(|entry| &entry.value)
    }

    /// Replace a matching key and its value together. Failed footprint or tree admission leaves the original root and footprint unchanged.
    pub fn insert(&mut self, key: K, value: V, payload_bytes: usize) -> Result<(), MemoryError> {
        let previous = self.values.get(&key).map_or(0, |entry| entry.payload_bytes);
        let total = self
            .payload_bytes
            .checked_sub(previous)
            .and_then(|bytes| bytes.checked_add(payload_bytes))
            .ok_or(MemoryError::SizeOverflow)?;
        self.values.try_insert(
            key,
            Entry {
                value,
                payload_bytes,
            },
        )?;
        self.payload_bytes = total;
        Ok(())
    }

    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Result<bool, MemoryError>
    where
        K: Borrow<Q>,
    {
        let Some(previous) = self.values.get(key) else {
            return Ok(false);
        };
        let total = self.payload_bytes - previous.payload_bytes;
        self.values = self.values.with_remove(key)?;
        self.payload_bytes = total;
        Ok(true)
    }
}

impl<'a, K, V> IntoIterator for &'a RetainedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = RetainedMapIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// An immutable root without a construction-budget handle or a mutable source owner.
pub struct RetainedMapSnapshot<K, V> {
    values: BudgetedSharedMapSnapshot<K, Entry<V>>,
}

impl<K, V> RetainedMapSnapshot<K, V> {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> RetainedMapIter<'_, K, V> {
        RetainedMapIter(self.values.iter())
    }
}

impl<K: Ord, V> RetainedMapSnapshot<K, V> {
    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.values.get(key).map(|entry| &entry.value)
    }
}

impl<'a, K, V> IntoIterator for &'a RetainedMapSnapshot<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = RetainedMapIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Ordered borrowed entries without an allocation or an additional retained root.
pub struct RetainedMapIter<'a, K, V>(BudgetedSharedMapIter<'a, K, Entry<V>>);

impl<'a, K, V> Iterator for RetainedMapIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(key, entry)| (key, &entry.value))
    }
}

#[cfg(test)]
mod tests;
