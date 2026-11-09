//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL definitions share immutable durable and temporary roots with independent capture admission.

use crate::SQLError;
use std::{borrow::Borrow, iter::Peekable};
use uqa_core::{
    memory::{
        Budgeted, MemoryBudget, MemoryError, Produced, ProductionControl, RetainedMap,
        RetainedMapIter, RetainedMapSnapshot,
    },
    CancellationToken,
};

mod image;
pub(crate) use image::DefinitionEntry;
pub(crate) use image::DefinitionImage;
mod serialization;

/// Borrowed catalog access shared by native generations and legacy decoded maps during validation.
pub trait NamedDefinitionView<T, K: DefinitionKey = String> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a T)>
    where
        T: 'a,
        K: 'a;
    fn get(&self, name: &K::View) -> Option<&T>;
    fn contains_key(&self, name: &K::View) -> bool {
        self.get(name).is_some()
    }
    fn values<'a>(&'a self) -> impl Iterator<Item = &'a T>
    where
        T: 'a,
        K: 'a,
    {
        self.iter().map(|(_, value)| value)
    }
}

impl<T, K: DefinitionKey> NamedDefinitionView<T, K> for std::collections::BTreeMap<K, T> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a T)>
    where
        T: 'a,
        K: 'a,
    {
        self.iter()
    }
    fn get(&self, name: &K::View) -> Option<&T> {
        self.get(name)
    }
}

impl<T, K: DefinitionKey> NamedDefinitionView<T, K> for DefinitionRegistry<K, T> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a T)>
    where
        T: 'a,
        K: 'a,
    {
        self.iter()
    }
    fn get(&self, name: &K::View) -> Option<&T> {
        self.get(name)
    }
}

impl<T, K: DefinitionKey> NamedDefinitionView<T, K> for CapturedDefinitions<K, T> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a T)>
    where
        T: 'a,
        K: 'a,
    {
        self.iter()
    }
    fn get(&self, name: &K::View) -> Option<&T> {
        self.get(name)
    }
}

impl<T, K: DefinitionKey, V: NamedDefinitionView<T, K>> NamedDefinitionView<T, K>
    for std::sync::Arc<V>
{
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a T)>
    where
        T: 'a,
        K: 'a,
    {
        self.as_ref().iter()
    }
    fn get(&self, name: &K::View) -> Option<&T> {
        self.as_ref().get(name)
    }
}

impl<T, K: DefinitionKey, V: NamedDefinitionView<T, K>> NamedDefinitionView<T, K>
    for uqa_core::memory::RetainedValue<V>
{
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a T)>
    where
        T: 'a,
        K: 'a,
    {
        self.as_ref().iter()
    }
    fn get(&self, name: &K::View) -> Option<&T> {
        self.as_ref().get(name)
    }
}

pub(crate) mod sealed {
    pub trait Definition {}
    pub trait Key {}
}

/// Native definitions with exhaustive SQL-owned copying and durable eligibility.
pub trait StoredDefinition<K: ?Sized = str>: sealed::Definition + Clone {
    fn is_temporary(&self, key: &K) -> bool;
    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError>;
    /// Immutable child handles retain their own native leases; captures must also fund their complete payloads.
    fn retained_children_bytes(&self) -> Result<usize, SQLError> {
        Ok(0)
    }
}

/// Native keys expose only separately allocated payloads; inline key storage belongs to the retained map node.
pub trait DefinitionKey: sealed::Key + Ord + Borrow<Self::View> {
    type View: Ord + ?Sized + ToOwned<Owned = Self>;
    fn payload_bytes(&self) -> usize;
}

impl sealed::Key for String {}
impl DefinitionKey for String {
    type View = str;
    fn payload_bytes(&self) -> usize {
        self.capacity()
    }
}

pub type NamedDefinitions<T> = DefinitionRegistry<String, T>;
pub type CapturedNamedDefinitions<T> = CapturedDefinitions<String, T>;
pub type NamedDefinitionIter<'a, T> = DefinitionIter<'a, String, T>;

/// One canonical entry per key. Temporary definitions live in their own tree and never belong to a durable capture. Native readers merge the two ordered roots without allocating; mutation publishes both roots only after all admission succeeds.
pub struct DefinitionRegistry<K, T> {
    durable: RetainedMap<K, DefinitionEntry<T>>,
    temporary: RetainedMap<K, DefinitionEntry<T>>,
}

impl<K: Ord, T> Clone for DefinitionRegistry<K, T> {
    fn clone(&self) -> Self {
        Self {
            durable: self.durable.clone(),
            temporary: self.temporary.clone(),
        }
    }
}

impl<K: Ord, T: PartialEq> PartialEq for DefinitionRegistry<K, T> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<K: Ord, T: Eq> Eq for DefinitionRegistry<K, T> {}

impl<K: Ord, T> Default for DefinitionRegistry<K, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord, T> DefinitionRegistry<K, T> {
    pub fn new() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }

    /// The native allowance funds definition copies and immutable tree construction; each capture independently funds the selected durable root.
    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self {
            durable: RetainedMap::new(memory),
            temporary: RetainedMap::new(memory),
        }
    }

    /// Select the destination allowance for future edits. Original entries and all captured predecessors keep their own reservations.
    pub(crate) fn into_mutation_budget(mut self, memory: &MemoryBudget) -> Self {
        self.durable = self.durable.into_mutation_budget(memory);
        self.temporary = self.temporary.into_mutation_budget(memory);
        self
    }

    /// Observe both immutable membership roots without comparing or copying declarations.
    pub fn shares_roots(&self, other: &Self) -> bool {
        self.durable.shares_root(&other.durable) && self.temporary.shares_root(&other.temporary)
    }

    pub fn len(&self) -> usize {
        self.durable.len() + self.temporary.len()
    }

    pub fn budget(&self) -> &MemoryBudget {
        self.durable.budget()
    }
    pub fn is_empty(&self) -> bool {
        self.durable.is_empty() && self.temporary.is_empty()
    }

    pub fn get<Q: Ord + ?Sized>(&self, name: &Q) -> Option<&T>
    where
        K: Borrow<Q>,
    {
        self.durable
            .get(name)
            .or_else(|| self.temporary.get(name))
            .map(|value| &**value)
    }

    pub fn contains_key<Q: Ord + ?Sized>(&self, name: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get(name).is_some()
    }

    /// Shared immutable definitions need no serialization to establish unchanged content.
    pub fn shares_definition<Q: Ord + ?Sized>(&self, name: &Q, other: &Self) -> bool
    where
        K: Borrow<Q>,
    {
        matches!((self.get(name), other.get(name)), (Some(left), Some(right)) if std::ptr::eq(left, right))
    }

    pub fn iter(&self) -> DefinitionIter<'_, K, T> {
        DefinitionIter {
            durable: self.durable.iter().peekable(),
            temporary: self.temporary.iter().peekable(),
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(name, _)| name)
    }
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.iter().map(|(_, value)| value)
    }

    pub fn clear(&mut self) {
        self.durable.clear();
        self.temporary.clear();
    }

    pub fn temporary_only(&self) -> Self {
        let mut temporary = self.clone();
        temporary.durable.clear();
        temporary
    }

    pub fn remove<Q: Ord + ?Sized>(&mut self, name: &Q) -> Result<bool, SQLError>
    where
        K: Borrow<Q>,
    {
        let mut durable = self.durable.clone();
        let mut temporary = self.temporary.clone();
        let removed_durable = durable.remove(name)?;
        let removed_temporary = temporary.remove(name)?;
        self.durable = durable;
        self.temporary = temporary;
        Ok(removed_durable || removed_temporary)
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&K, &T) -> bool) -> Result<(), SQLError> {
        let mut retained = self.clone();
        for (name, value) in self.iter() {
            if !keep(name, value) {
                retained.remove(name)?;
            }
        }
        *self = retained;
        Ok(())
    }

    /// Capture selects the prepublished durable root in constant work. It neither visits definitions nor retains the temporary tree. This retention boundary requires an admitted control.
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedDefinitions<K, T>>, SQLError> {
        control.check()?;
        let memory = control.budget().ok_or_else(|| {
            SQLError::Internal("definition capture requires an admitted control".into())
        })?;
        let (root, memory) = self.durable.capture(memory)?.into_parts();
        Ok(control.finish(CapturedDefinitions { root }, Some(memory))?)
    }
}

impl<K: DefinitionKey, T: StoredDefinition<K::View>> DefinitionRegistry<K, T> {
    /// Move decoded payloads into their native tree without copying or releasing their admission. The image owner has already validated keys and definitions.
    pub(crate) fn insert_image_entry(
        &mut self,
        name: Budgeted<K>,
        value: Budgeted<T>,
    ) -> Result<(), SQLError> {
        assert!(self.budget().shares_allowance(name.budget()));
        assert!(self.budget().shares_allowance(value.budget()));
        let children = value.retained_children_bytes()?;
        let bytes = name
            .payload_bytes()
            .checked_add(value.reserved_bytes())
            .and_then(|bytes| bytes.checked_add(children))
            .ok_or(MemoryError::SizeOverflow)?;
        let (name, key_memory) = name.into_parts();
        let (value, mut memory) = value.into_parts();
        memory.absorb(key_memory);
        self.publish_entry(
            name,
            DefinitionEntry::Native(Budgeted::new(value, memory)),
            bytes,
        )
    }

    pub(crate) fn last_durable_key(&self) -> Option<&K> {
        self.durable
            .last_before::<K>(std::ops::Bound::Unbounded)
            .map(|(key, _)| key)
    }

    pub fn insert(&mut self, name: K, value: T) -> Result<(), SQLError> {
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(self.durable.budget(), &cancellation, &cancellation);
        let value = value
            .copy_definition(&control)?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("native definitions use admitted construction"));
        let children = value.retained_children_bytes()?;
        let bytes = name
            .payload_bytes()
            .checked_add(value.reserved_bytes())
            .and_then(|bytes| bytes.checked_add(children))
            .ok_or(MemoryError::SizeOverflow)?;
        self.publish_entry(name, DefinitionEntry::Native(value), bytes)
    }

    fn publish_entry(
        &mut self,
        name: K,
        value: DefinitionEntry<T>,
        bytes: usize,
    ) -> Result<(), SQLError> {
        // The tuple retires the key before the value's lease, including imported key payloads.
        let entry = (name, value);
        let temporary = entry.1.is_temporary(entry.0.borrow());
        let mut durable = self.durable.clone();
        let mut session = self.temporary.clone();
        if temporary {
            durable.remove(entry.0.borrow())?;
            let (name, value) = entry;
            session.insert(name, value, bytes)?;
        } else {
            session.remove(entry.0.borrow())?;
            let (name, value) = entry;
            durable.insert(name, value, bytes)?;
        }
        self.durable = durable;
        self.temporary = session;
        Ok(())
    }

    /// Edit only the selected definition, keeping all other payloads shared. A failed edit or replacement leaves the original roots unchanged.
    pub fn edit<R>(
        &mut self,
        name: &K::View,
        edit: impl FnOnce(&mut T) -> Result<R, SQLError>,
    ) -> Result<Option<R>, SQLError> {
        let Some(mut value) = self.get(name).cloned() else {
            return Ok(None);
        };
        let result = edit(&mut value)?;
        self.insert(name.to_owned(), value)?;
        Ok(Some(result))
    }

    pub fn try_from_iter(values: impl IntoIterator<Item = (K, T)>) -> Result<Self, SQLError> {
        let mut result = Self::new();
        for (name, value) in values {
            result.insert(name, value)?;
        }
        Ok(result)
    }
}

impl<K: Ord + Borrow<Q>, T, Q: Ord + ?Sized> std::ops::Index<&Q> for DefinitionRegistry<K, T> {
    type Output = T;
    fn index(&self, key: &Q) -> &T {
        self.get(key).expect("catalog definition key exists")
    }
}

/// Ordered native view over the two disjoint canonical partitions.
pub struct DefinitionIter<'a, K, T> {
    durable: Peekable<RetainedMapIter<'a, K, DefinitionEntry<T>>>,
    temporary: Peekable<RetainedMapIter<'a, K, DefinitionEntry<T>>>,
}

impl<'a, K: Ord, T> Iterator for DefinitionIter<'a, K, T> {
    type Item = (&'a K, &'a T);
    fn next(&mut self) -> Option<Self::Item> {
        let entry = match (self.durable.peek(), self.temporary.peek()) {
            (Some((left, _)), Some((right, _))) if left < right => self.durable.next(),
            (_, Some(_)) => self.temporary.next(),
            (Some(_), None) => self.durable.next(),
            (None, None) => None,
        };
        entry.map(|(name, value)| (name, &**value))
    }
}

impl<'a, K: Ord, T> IntoIterator for &'a DefinitionRegistry<K, T> {
    type Item = (&'a K, &'a T);
    type IntoIter = DefinitionIter<'a, K, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Immutable durable definitions; the enclosing produced owner retains their independent admission.
pub struct CapturedDefinitions<K, T> {
    root: RetainedMapSnapshot<K, DefinitionEntry<T>>,
}

impl<K: Ord, T> CapturedDefinitions<K, T> {
    pub fn len(&self) -> usize {
        self.root.len()
    }
    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }
    pub fn get<Q: Ord + ?Sized>(&self, name: &Q) -> Option<&T>
    where
        K: Borrow<Q>,
    {
        self.root.get(name).map(|value| &**value)
    }
    pub fn iter(&self) -> CapturedDefinitionIter<'_, K, T> {
        CapturedDefinitionIter {
            inner: self.root.iter(),
        }
    }
}

/// Allocation-free ordered access to captured native definitions.
pub struct CapturedDefinitionIter<'a, K, T> {
    inner: RetainedMapIter<'a, K, DefinitionEntry<T>>,
}

impl<'a, K, T> Iterator for CapturedDefinitionIter<'a, K, T> {
    type Item = (&'a K, &'a T);
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(key, value)| (key, &**value))
    }
}

impl<'a, K: Ord, T> IntoIterator for &'a CapturedDefinitions<K, T> {
    type Item = (&'a K, &'a T);
    type IntoIter = CapturedDefinitionIter<'a, K, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
