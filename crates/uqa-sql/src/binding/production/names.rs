//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lexical binding names own their admitted text and exact ordered nodes independently.

use crate::SQLError;
use std::{borrow::Borrow, cmp::Ordering, collections::BTreeMap};
use uqa_core::memory::{BudgetedMapIter, OwnedMap, Produced, ProductionControl};

pub struct BindingName(Produced<String>);
impl Borrow<str> for BindingName {
    fn borrow(&self) -> &str {
        &self.0
    }
}
impl PartialEq for BindingName {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq(&other.0)
    }
}
impl Eq for BindingName {}
impl PartialOrd for BindingName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for BindingName {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.as_str().cmp(other.0.as_str())
    }
}

/// Each entry releases its name and node when removed. Values retain their own immutable metadata owners.
pub struct BindingMap<V> {
    entries: OwnedMap<BindingName, V>,
}
impl<V> Default for BindingMap<V> {
    fn default() -> Self {
        Self {
            entries: OwnedMap::new(),
        }
    }
}
impl<V> BindingMap<V> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn get(&self, name: &str) -> Option<&V> {
        self.entries.get(name)
    }
    pub fn get_mut(&mut self, name: &str) -> Option<&mut V> {
        self.entries.get_mut(name)
    }
    pub fn contains_key(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }
    pub fn remove(&mut self, name: &str) -> Option<V> {
        self.entries.remove(name)
    }
    pub(in crate::binding) fn extract(&mut self, name: &str) -> Self {
        Self {
            entries: self.entries.extract(name),
        }
    }
    pub(in crate::binding) fn append(&mut self, entries: Self) {
        self.entries.append(entries.entries);
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&String, &mut V) -> bool) {
        self.entries.retain(|name, value| keep(&name.0, value));
    }
    pub fn iter(&self) -> BindingIter<'_, V> {
        BindingIter(self.entries.iter())
    }
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &String> {
        self.iter().map(|(name, _)| name)
    }
    pub fn values(&self) -> impl ExactSizeIterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }
    pub fn insert(&mut self, name: String, value: V) -> Option<V> {
        self.entries.insert(
            BindingName(
                ProductionControl::uncontrolled()
                    .finish(name, None)
                    .expect("ordinary binding name"),
            ),
            value,
        )
    }
    pub fn insert_with_control(
        &mut self,
        name: &str,
        value: V,
        control: &ProductionControl<'_>,
    ) -> Result<Option<V>, SQLError> {
        control.check()?;
        if let Some(previous) = self.get_mut(name) {
            return Ok(Some(std::mem::replace(previous, value)));
        }
        let name = BindingName(control.copy_text(name)?);
        Ok(match control.budget() {
            Some(budget) => self.entries.insert_budgeted(name, value, budget)?,
            None => self.entries.insert(name, value),
        })
    }
    /// The callback decides how each value shares or independently produces its own payload; this map never assumes Clone admits an allocating value.
    pub fn copy_with_control(
        &self,
        control: &ProductionControl<'_>,
        mut value: impl FnMut(&V) -> Result<V, SQLError>,
    ) -> Result<Self, SQLError> {
        let mut output = Self::new();
        for (name, item) in self {
            control.check()?;
            output.insert_with_control(name, value(item)?, control)?;
        }
        Ok(output)
    }
}
impl<V: Clone> Clone for BindingMap<V> {
    fn clone(&self) -> Self {
        self.iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
}
impl<V> FromIterator<(String, V)> for BindingMap<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(entries: I) -> Self {
        let mut output = Self::new();
        for (name, value) in entries {
            output.insert(name, value);
        }
        output
    }
}
impl<V> From<BTreeMap<String, V>> for BindingMap<V> {
    fn from(entries: BTreeMap<String, V>) -> Self {
        entries.into_iter().collect()
    }
}
impl<V> std::ops::Index<&str> for BindingMap<V> {
    type Output = V;
    fn index(&self, name: &str) -> &V {
        self.get(name).expect("bound lexical name")
    }
}
pub struct BindingIter<'a, V>(BudgetedMapIter<'a, BindingName, V>);
impl<'a, V> Iterator for BindingIter<'a, V> {
    type Item = (&'a String, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(name, value)| (&*name.0, value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl<V> ExactSizeIterator for BindingIter<'_, V> {}
impl<V> std::iter::FusedIterator for BindingIter<'_, V> {}
impl<'a, V> IntoIterator for &'a BindingMap<V> {
    type Item = (&'a String, &'a V);
    type IntoIter = BindingIter<'a, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Default, Clone)]
pub struct BindingNameSet(BindingMap<()>);
impl BindingNameSet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }
    pub fn insert(&mut self, name: String) -> bool {
        self.0.insert(name, ()).is_none()
    }
    pub fn remove(&mut self, name: &str) -> bool {
        self.0.remove(name).is_some()
    }
    pub(in crate::binding) fn extract(&mut self, name: &str) -> Self {
        Self(self.0.extract(name))
    }
    pub(in crate::binding) fn append(&mut self, entries: Self) {
        self.0.append(entries.0);
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &String> {
        self.0.keys()
    }
    pub fn insert_with_control(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.0
            .insert_with_control(name, (), control)
            .map(|old| old.is_none())
    }
    pub fn copy_with_control(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        self.0.copy_with_control(control, |()| Ok(())).map(Self)
    }
}
impl FromIterator<String> for BindingNameSet {
    fn from_iter<I: IntoIterator<Item = String>>(entries: I) -> Self {
        Self(entries.into_iter().map(|name| (name, ())).collect())
    }
}
