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

mod serialization;

/// Borrowed catalog access shared by native generations and legacy decoded maps during validation.
pub trait NamedDefinitionView<T> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a String, &'a T)>
    where
        T: 'a;
    fn get(&self, name: &str) -> Option<&T>;
    fn values<'a>(&'a self) -> impl Iterator<Item = &'a T>
    where
        T: 'a,
    {
        self.iter().map(|(_, value)| value)
    }
}

impl<T> NamedDefinitionView<T> for std::collections::BTreeMap<String, T> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a String, &'a T)>
    where
        T: 'a,
    {
        self.iter()
    }
    fn get(&self, name: &str) -> Option<&T> {
        self.get(name)
    }
}

impl<T> NamedDefinitionView<T> for NamedDefinitions<T> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a String, &'a T)>
    where
        T: 'a,
    {
        self.iter()
    }
    fn get(&self, name: &str) -> Option<&T> {
        self.get(name)
    }
}

impl<T, V: NamedDefinitionView<T>> NamedDefinitionView<T> for std::sync::Arc<V> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a String, &'a T)>
    where
        T: 'a,
    {
        self.as_ref().iter()
    }
    fn get(&self, name: &str) -> Option<&T> {
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
    durable: RetainedMap<K, Budgeted<T>>,
    temporary: RetainedMap<K, Budgeted<T>>,
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

    pub fn len(&self) -> usize {
        self.durable.len() + self.temporary.len()
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
    pub fn insert(&mut self, name: K, value: T) -> Result<(), SQLError> {
        let temporary = value.is_temporary(name.borrow());
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(self.durable.budget(), &cancellation, &cancellation);
        let value = value
            .copy_definition(&control)?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("native definitions use admitted construction"));
        let bytes = name
            .payload_bytes()
            .checked_add(value.reserved_bytes())
            .ok_or(MemoryError::SizeOverflow)?;
        let mut durable = self.durable.clone();
        let mut session = self.temporary.clone();
        if temporary {
            durable.remove(name.borrow())?;
            session.insert(name, value, bytes)?;
        } else {
            session.remove(name.borrow())?;
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
    durable: Peekable<RetainedMapIter<'a, K, Budgeted<T>>>,
    temporary: Peekable<RetainedMapIter<'a, K, Budgeted<T>>>,
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
    root: RetainedMapSnapshot<K, Budgeted<T>>,
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
    pub fn iter(&self) -> impl Iterator<Item = (&K, &T)> {
        self.root.iter().map(|(name, value)| (name, &**value))
    }
}
