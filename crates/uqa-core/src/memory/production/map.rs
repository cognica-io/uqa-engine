//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered copy-only scratch keeps each allocated node with its existing reservation owner.

use super::ProductionControl;
use crate::{
    memory::{BudgetedMap, BudgetedMapIter, OwnedMap},
    ValueRetentionError,
};
use std::borrow::Borrow;

enum Buffer<K, V> {
    Ordinary(OwnedMap<K, V>),
    Controlled(BudgetedMap<K, V>),
}

/// Copy-only keys and values contain no independently owned allocation. Controlled insertion reserves each node before construction; replacement and removal reuse the ordered map's existing ownership operations.
pub struct ProductionMap<'a, K: Copy, V: Copy> {
    buffer: Buffer<K, V>,
    control: ProductionControl<'a>,
}

impl<'a, K: Ord + Copy, V: Copy> ProductionMap<'a, K, V> {
    pub fn new(control: ProductionControl<'a>) -> Self {
        let buffer = control.budget().map_or_else(
            || Buffer::Ordinary(OwnedMap::new()),
            |budget| Buffer::Controlled(BudgetedMap::new(budget)),
        );
        Self { buffer, control }
    }

    pub fn len(&self) -> usize {
        match &self.buffer {
            Buffer::Ordinary(map) => map.len(),
            Buffer::Controlled(map) => map.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        match &self.buffer {
            Buffer::Ordinary(map) => map.get(key),
            Buffer::Controlled(map) => map.get(key),
        }
    }

    pub fn insert_copy(&mut self, key: K, value: V) -> Result<Option<V>, ValueRetentionError> {
        self.control.check()?;
        Ok(match &mut self.buffer {
            Buffer::Ordinary(map) => map.insert(key, value),
            Buffer::Controlled(map) => map.insert(key, value)?,
        })
    }

    /// Removing scratch does not require admission or cancellation permission; the node's allocation is freed before its reservation is released.
    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        match &mut self.buffer {
            Buffer::Ordinary(map) => map.remove(key),
            Buffer::Controlled(map) => map.remove(key),
        }
    }

    pub fn iter(&self) -> BudgetedMapIter<'_, K, V> {
        match &self.buffer {
            Buffer::Ordinary(map) => map.iter(),
            Buffer::Controlled(map) => map.iter(),
        }
    }
}

impl<'m, K: Ord + Copy, V: Copy> IntoIterator for &'m ProductionMap<'_, K, V> {
    type Item = (&'m K, &'m V);
    type IntoIter = BudgetedMapIter<'m, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests;
