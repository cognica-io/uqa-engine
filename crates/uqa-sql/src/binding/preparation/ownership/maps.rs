//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered analysis identities own exact map nodes; deleting an observation retires its payload and node immediately.

use crate::SQLError;
use std::borrow::Borrow;
use uqa_core::memory::{
    BudgetedMapIter, OwnedMap, OwnedMapIntoIter, OwnedProductionControl, ProductionControl,
};

pub(in crate::binding) struct AnalysisMap<K, V> {
    entries: OwnedMap<K, V>,
    control: OwnedProductionControl,
}

impl<K: Ord, V> Default for AnalysisMap<K, V> {
    fn default() -> Self {
        Self::new(&ProductionControl::uncontrolled())
    }
}

impl<K: Ord, V> AnalysisMap<K, V> {
    pub(in crate::binding) fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            entries: OwnedMap::new(),
            control: control.capture(),
        }
    }
    pub(in crate::binding) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub(in crate::binding) fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.entries.get(key)
    }
    pub(in crate::binding) fn get_mut<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        self.entries.get_mut(key)
    }
    pub(in crate::binding) fn contains_key<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.entries.contains_key(key)
    }
    pub(in crate::binding) fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        self.entries.remove(key)
    }
    pub(in crate::binding) fn values(&self) -> impl ExactSizeIterator<Item = &V> {
        self.entries.values()
    }
    pub(in crate::binding) fn insert(&mut self, key: K, value: V) -> Result<Option<V>, SQLError> {
        let control = self.control.control();
        control.check()?;
        Ok(match control.budget() {
            Some(budget) => self.entries.insert_budgeted(key, value, budget)?,
            None => self.entries.insert(key, value),
        })
    }
    pub(in crate::binding) fn insert_if_missing(
        &mut self,
        key: K,
        value: impl FnOnce() -> Result<V, SQLError>,
    ) -> Result<(), SQLError> {
        self.control.control().check()?;
        if !self.entries.contains_key(&key) {
            self.insert(key, value()?)?;
        }
        Ok(())
    }
    pub(in crate::binding) fn append(&mut self, other: Self) {
        self.entries.append(other.entries);
    }
}

impl<K, V> IntoIterator for AnalysisMap<K, V> {
    type Item = (K, V);
    type IntoIter = OwnedMapIntoIter<K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a, K: Ord, V> IntoIterator for &'a AnalysisMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = BudgetedMapIter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}
