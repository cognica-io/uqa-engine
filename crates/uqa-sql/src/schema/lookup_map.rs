//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary mutation and admitted immutable lookup share column-key hashing and equality.

use std::{
    borrow::Borrow,
    collections::{hash_map, HashMap},
    fmt,
    hash::Hash,
    sync::Arc,
};
use uqa_core::memory::{BudgetedHashIndex, BudgetedHashIndexIter, Produced, ProductionControl};

type Ordinary<K, V> = HashMap<K, V, ahash::RandomState>;
type Admitted<K, V> = Arc<Produced<BudgetedHashIndex<K, V, ahash::RandomState>>>;

pub(crate) enum SchemaMap<K, V> {
    Ordinary(Ordinary<K, V>),
    Admitted(Admitted<K, V>),
}

impl<K, V> SchemaMap<K, V> {
    pub(crate) fn with_hasher(hasher: ahash::RandomState) -> Self {
        Self::Ordinary(HashMap::with_hasher(hasher))
    }
    pub(crate) fn with_capacity_and_hasher(capacity: usize, hasher: ahash::RandomState) -> Self {
        Self::Ordinary(HashMap::with_capacity_and_hasher(capacity, hasher))
    }
    pub(crate) fn from_admitted(
        index: BudgetedHashIndex<K, V, ahash::RandomState>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, crate::SQLError> {
        Ok(Self::Admitted(
            control
                .finish(index, control.empty_reservation())?
                .into_shared(control)?,
        ))
    }
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Ordinary(map) => map.len(),
            Self::Admitted(map) => map.len(),
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(crate) fn iter(&self) -> SchemaMapIter<'_, K, V> {
        match self {
            Self::Ordinary(map) => SchemaMapIter::Ordinary(map.iter()),
            Self::Admitted(map) => SchemaMapIter::Admitted(map.iter()),
        }
    }
    pub(crate) fn keys(&self) -> impl ExactSizeIterator<Item = &K> {
        self.iter().map(|(key, _)| key)
    }
    pub(crate) fn values(&self) -> impl ExactSizeIterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }
}

impl<K: Eq + Hash, V> SchemaMap<K, V> {
    pub(super) fn from_produced(
        entries: impl IntoIterator<Item = Result<(Produced<K>, Produced<V>), crate::SQLError>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, crate::SQLError> {
        control.check()?;
        if let Some(budget) = control.budget() {
            let entries = entries.into_iter().map(|entry| {
                let (key, value) = entry?;
                Ok((
                    key.into_budgeted()
                        .unwrap_or_else(|_| panic!("admitted schema key")),
                    value
                        .into_budgeted()
                        .unwrap_or_else(|_| panic!("admitted schema value")),
                ))
            });
            let index =
                BudgetedHashIndex::build(budget, ahash::RandomState::default(), entries, || {
                    control.check().map_err(crate::SQLError::from)
                })?;
            Self::from_admitted(index, control)
        } else {
            let mut map = HashMap::with_hasher(ahash::RandomState::default());
            for entry in entries {
                let (key, value) = entry?;
                map.insert(
                    key.into_uncontrolled()
                        .unwrap_or_else(|_| panic!("ordinary schema key")),
                    value
                        .into_uncontrolled()
                        .unwrap_or_else(|_| panic!("ordinary schema value")),
                );
            }
            Ok(Self::Ordinary(map))
        }
    }

    pub(crate) fn get<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        match self {
            Self::Ordinary(map) => map.get(key),
            Self::Admitted(map) => map.get(key),
        }
    }
    pub(crate) fn contains_key<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get(key).is_some()
    }
}

impl<K: Eq + Hash + Clone, V: Clone> SchemaMap<K, V> {
    /// The ordinary API produces its own mutable generation. Controlled producers publish completed indexes through `from_admitted` and never allocate through these infallible mutation methods.
    fn ordinary_mutation(&mut self) -> &mut Ordinary<K, V> {
        if let Self::Admitted(index) = self {
            *self = Self::Ordinary(
                index
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            );
        }
        let Self::Ordinary(map) = self else {
            unreachable!("ordinary generation was produced")
        };
        map
    }
    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.ordinary_mutation().insert(key, value)
    }
    pub(crate) fn remove<Q: Eq + Hash + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        self.ordinary_mutation().remove(key)
    }
    pub(crate) fn retain(&mut self, keep: impl FnMut(&K, &mut V) -> bool) {
        self.ordinary_mutation().retain(keep);
    }
    pub(crate) fn entry(&mut self, key: K) -> hash_map::Entry<'_, K, V> {
        self.ordinary_mutation().entry(key)
    }
}

impl<K, V> Default for SchemaMap<K, V> {
    fn default() -> Self {
        Self::with_hasher(ahash::RandomState::default())
    }
}
impl<K: Clone, V: Clone> Clone for SchemaMap<K, V> {
    fn clone(&self) -> Self {
        match self {
            Self::Ordinary(map) => Self::Ordinary(map.clone()),
            Self::Admitted(map) => Self::Admitted(map.clone()),
        }
    }
}
impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for SchemaMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}
impl<K: Eq + Hash, V: PartialEq> PartialEq for SchemaMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(key, value)| other.get(key) == Some(value))
    }
}
impl<K: Eq + Hash, V: Eq> Eq for SchemaMap<K, V> {}
impl<K: Eq + Hash, V> FromIterator<(K, V)> for SchemaMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(entries: T) -> Self {
        Self::Ordinary(entries.into_iter().collect())
    }
}
impl<K: Eq + Hash + Clone, V: Clone> Extend<(K, V)> for SchemaMap<K, V> {
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, entries: T) {
        self.ordinary_mutation().extend(entries);
    }
}
impl<K: Eq + Hash + Borrow<Q>, V, Q: Eq + Hash + ?Sized> std::ops::Index<&Q> for SchemaMap<K, V> {
    type Output = V;
    fn index(&self, key: &Q) -> &Self::Output {
        self.get(key).expect("missing schema map key")
    }
}
impl<K: Eq + Hash + Clone, V: Clone> IntoIterator for SchemaMap<K, V> {
    type Item = (K, V);
    type IntoIter = hash_map::IntoIter<K, V>;
    fn into_iter(self) -> Self::IntoIter {
        match self {
            Self::Ordinary(map) => map.into_iter(),
            Self::Admitted(map) => map
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Ordinary<_, _>>()
                .into_iter(),
        }
    }
}
impl<'a, K, V> IntoIterator for &'a SchemaMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = SchemaMapIter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub(crate) enum SchemaMapIter<'a, K, V> {
    Ordinary(hash_map::Iter<'a, K, V>),
    Admitted(BudgetedHashIndexIter<'a, K, V>),
}
impl<'a, K, V> Iterator for SchemaMapIter<'a, K, V> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Ordinary(iter) => iter.next(),
            Self::Admitted(iter) => iter.next(),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Ordinary(iter) => iter.size_hint(),
            Self::Admitted(iter) => iter.size_hint(),
        }
    }
}
impl<K, V> ExactSizeIterator for SchemaMapIter<'_, K, V> {}
impl<K, V> std::iter::FusedIterator for SchemaMapIter<'_, K, V> {}
