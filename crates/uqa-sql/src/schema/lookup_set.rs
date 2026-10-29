//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::lookup_map::{SchemaMap, SchemaMapIter};
use std::{borrow::Borrow, fmt, hash::Hash};
use uqa_core::memory::{Produced, ProductionControl};

pub(crate) struct SchemaSet<T>(SchemaMap<T, ()>);

impl<T> SchemaSet<T> {
    pub(crate) fn with_hasher(hasher: ahash::RandomState) -> Self {
        Self(SchemaMap::with_hasher(hasher))
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn iter(&self) -> SchemaSetIter<'_, T> {
        SchemaSetIter(self.0.iter())
    }
}

impl<T: Eq + Hash> SchemaSet<T> {
    pub(super) fn from_produced(
        entries: impl IntoIterator<Item = Result<Produced<T>, crate::SQLError>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, crate::SQLError> {
        Ok(Self(SchemaMap::from_produced(
            entries
                .into_iter()
                .map(|entry| Ok((entry?, control.finish((), control.empty_reservation())?))),
            control,
        )?))
    }
    pub(crate) fn contains<Q: Eq + Hash + ?Sized>(&self, value: &Q) -> bool
    where
        T: Borrow<Q>,
    {
        self.0.contains_key(value)
    }
}

impl<T: Eq + Hash + Clone> SchemaSet<T> {
    pub(crate) fn insert(&mut self, value: T) -> bool {
        self.0.insert(value, ()).is_none()
    }
    pub(crate) fn remove<Q: Eq + Hash + ?Sized>(&mut self, value: &Q) -> bool
    where
        T: Borrow<Q>,
    {
        self.0.remove(value).is_some()
    }
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        self.0.retain(|key, ()| keep(key));
    }
}

impl<T> Default for SchemaSet<T> {
    fn default() -> Self {
        Self(SchemaMap::default())
    }
}
impl<T: Clone> Clone for SchemaSet<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T: fmt::Debug> fmt::Debug for SchemaSet<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}
impl<T: Eq + Hash> PartialEq for SchemaSet<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<T: Eq + Hash> Eq for SchemaSet<T> {}
impl<T: Eq + Hash> FromIterator<T> for SchemaSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(entries: I) -> Self {
        Self(entries.into_iter().map(|entry| (entry, ())).collect())
    }
}
impl<T: Eq + Hash + Clone> Extend<T> for SchemaSet<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, entries: I) {
        self.0.extend(entries.into_iter().map(|entry| (entry, ())));
    }
}
impl<T: Eq + Hash + Clone> std::ops::BitOr for &SchemaSet<T> {
    type Output = SchemaSet<T>;
    fn bitor(self, other: Self) -> Self::Output {
        self.iter().chain(other.iter()).cloned().collect()
    }
}
impl<T: Eq + Hash + Clone> IntoIterator for SchemaSet<T> {
    type Item = T;
    type IntoIter = std::iter::Map<std::collections::hash_map::IntoIter<T, ()>, fn((T, ())) -> T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter().map(|(key, ())| key)
    }
}
impl<'a, T> IntoIterator for &'a SchemaSet<T> {
    type Item = &'a T;
    type IntoIter = SchemaSetIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
pub(crate) struct SchemaSetIter<'a, T>(SchemaMapIter<'a, T, ()>);
impl<'a, T> Iterator for SchemaSetIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(key, ())| key)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl<T> ExactSizeIterator for SchemaSetIter<'_, T> {}
impl<T> std::iter::FusedIterator for SchemaSetIter<'_, T> {}
