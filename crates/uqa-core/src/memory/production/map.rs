//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered copy-only scratch keeps each allocated node with its existing reservation owner.

use super::ProductionControl;
use crate::{
    memory::{BudgetedMapIter, MapKeyInput, MemoryError, OwnedMap},
    ValueRetentionError,
};
use std::borrow::Borrow;

/// Copy-only keys and values contain no independently owned allocation. Controlled insertion reserves each node before construction; replacement and removal reuse the ordered map's existing ownership operations.
pub struct ProductionMap<'a, K: Copy, V: Copy> {
    buffer: OwnedMap<K, V>,
    control: ProductionControl<'a>,
}

impl<'a, K: Ord + Copy, V: Copy> ProductionMap<'a, K, V> {
    pub fn new(control: ProductionControl<'a>) -> Self {
        let buffer = OwnedMap::new();
        Self { buffer, control }
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.buffer.get(key)
    }

    pub fn insert_copy(&mut self, key: K, value: V) -> Result<Option<V>, ValueRetentionError> {
        self.control.check()?;
        Ok(match self.control.budget() {
            None => self.buffer.insert(key, value),
            Some(budget) => self.buffer.insert_budgeted(key, value, budget)?,
        })
    }

    /// Borrow scratch through the existing retained comparison path, preserving both production cancellation owners.
    pub async fn get_from<Q: ?Sized + Sync, C: MapKeyInput<Q, K> + Send>(
        &self,
        key: &Q,
        input: &mut C,
    ) -> Result<Option<&V>, C::Error>
    where
        K: Sync,
        C::Error: From<ValueRetentionError>,
    {
        self.buffer
            .get_from(
                key,
                &mut Controlled {
                    control: self.control,
                    input,
                },
            )
            .await
    }

    /// Admit a new scratch node after retained comparisons complete. Interruption leaves every previous key, value and node lease unchanged.
    pub async fn insert_copy_from<C: MapKeyInput<K> + Send>(
        &mut self,
        key: K,
        value: V,
        input: &mut C,
    ) -> Result<Option<V>, C::Error>
    where
        K: Sync,
        C::Error: From<ValueRetentionError> + From<MemoryError>,
    {
        let key = self.control.finish(key, self.control.empty_reservation())?;
        self.buffer
            .insert_produced_key_from(
                key,
                value,
                &mut Controlled {
                    control: self.control,
                    input,
                },
            )
            .await
    }

    /// Removing scratch does not require admission or cancellation permission; the node's allocation is freed before its reservation is released.
    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        self.buffer.remove(key)
    }

    pub fn iter(&self) -> BudgetedMapIter<'_, K, V> {
        self.buffer.iter()
    }
}

struct Controlled<'a, 'c, C> {
    control: ProductionControl<'c>,
    input: &'a mut C,
}

impl<Q: ?Sized + Sync, K: Sync, C: MapKeyInput<Q, K> + Send> MapKeyInput<Q, K>
    for Controlled<'_, '_, C>
where
    C::Error: From<ValueRetentionError>,
{
    type Error = C::Error;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        self.control.check()?;
        self.input.checkpoint().await?;
        self.control.check()?;
        Ok(())
    }
    async fn compare(&mut self, query: &Q, stored: &K) -> Result<std::cmp::Ordering, Self::Error> {
        self.control.check()?;
        let order = self.input.compare(query, stored).await?;
        self.control.check()?;
        Ok(order)
    }
}

impl<'m, K: Ord + Copy, V: Copy> IntoIterator for &'m ProductionMap<'_, K, V> {
    type Item = (&'m K, &'m V);
    type IntoIter = BudgetedMapIter<'m, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<K: Ord + Copy, V: Copy> IntoIterator for ProductionMap<'_, K, V> {
    type Item = (K, V);
    type IntoIter = crate::memory::OwnedMapIntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.buffer.into_iter()
    }
}

#[cfg(test)]
mod tests;
