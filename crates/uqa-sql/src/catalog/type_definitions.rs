//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native type definitions use immutable, separately owned durable and temporary roots.

use crate::SQLError;
use std::iter::Peekable;
use uqa_core::{
    memory::{
        Budgeted, MemoryBudget, MemoryError, Produced, ProductionControl, RetainedMap,
        RetainedMapIter, RetainedMapSnapshot,
    },
    CancellationToken, RelationIdentity,
};

mod serialization;

/// Borrowed catalog access shared by native generations and legacy decoded maps during validation.
pub trait TypeDefinitionView<T> {
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

impl<T> TypeDefinitionView<T> for std::collections::BTreeMap<String, T> {
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

impl<T> TypeDefinitionView<T> for TypeDefinitions<T> {
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

impl<T, V: TypeDefinitionView<T>> TypeDefinitionView<T> for std::sync::Arc<V> {
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

mod sealed {
    pub trait Definition {}
}

/// Type forms whose native definitions have an exhaustive, non-evaluating SQL-owned copy producer.
pub trait StoredTypeDefinition: sealed::Definition + Clone {
    fn identity(&self) -> &RelationIdentity;
    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError>;
}

macro_rules! definitions {
    ($($ty:ty),+ $(,)?) => {$ (
        impl sealed::Definition for $ty {}
        impl StoredTypeDefinition for $ty {
            fn identity(&self) -> &RelationIdentity { &self.identity }
            fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
                self.capture_definition(control)
            }
        }
    )+};
}

definitions!(
    super::domain::StoredDomain,
    super::enum_type::StoredEnum,
    super::composite_type::StoredComposite,
    super::range_type::StoredRange,
    super::shell_type::StoredShellType,
);

/// One canonical entry per name. Temporary definitions live in their own tree and never belong to a durable capture. Native readers merge the two ordered roots without allocating; mutation publishes both roots only after all admission succeeds.
pub struct TypeDefinitions<T> {
    durable: RetainedMap<String, Budgeted<T>>,
    temporary: RetainedMap<String, Budgeted<T>>,
}

impl<T> Clone for TypeDefinitions<T> {
    fn clone(&self) -> Self {
        Self {
            durable: self.durable.clone(),
            temporary: self.temporary.clone(),
        }
    }
}

impl<T> Default for TypeDefinitions<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TypeDefinitions<T> {
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

    pub fn get(&self, name: &str) -> Option<&T> {
        self.durable
            .get(name)
            .or_else(|| self.temporary.get(name))
            .map(|value| &**value)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Shared immutable definitions need no serialization to establish unchanged content.
    pub fn shares_definition(&self, name: &str, other: &Self) -> bool {
        matches!((self.get(name), other.get(name)), (Some(left), Some(right)) if std::ptr::eq(left, right))
    }

    pub fn iter(&self) -> TypeDefinitionIter<'_, T> {
        TypeDefinitionIter {
            durable: self.durable.iter().peekable(),
            temporary: self.temporary.iter().peekable(),
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.iter().map(|(name, _)| name)
    }
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.iter().map(|(_, value)| value)
    }

    pub fn clear(&mut self) {
        self.durable.clear();
        self.temporary.clear();
    }

    pub fn remove(&mut self, name: &str) -> Result<bool, SQLError> {
        let mut durable = self.durable.clone();
        let mut temporary = self.temporary.clone();
        let removed_durable = durable.remove(name)?;
        let removed_temporary = temporary.remove(name)?;
        self.durable = durable;
        self.temporary = temporary;
        Ok(removed_durable || removed_temporary)
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&String, &T) -> bool) -> Result<(), SQLError> {
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
    ) -> Result<Produced<CapturedTypeDefinitions<T>>, SQLError> {
        control.check()?;
        let memory = control.budget().ok_or_else(|| {
            SQLError::Internal("type capture requires an admitted control".into())
        })?;
        let (root, memory) = self.durable.capture(memory)?.into_parts();
        Ok(control.finish(CapturedTypeDefinitions { root }, Some(memory))?)
    }
}

impl<T: StoredTypeDefinition> TypeDefinitions<T> {
    pub fn insert(&mut self, name: String, value: T) -> Result<(), SQLError> {
        let temporary =
            super::temporary_namespace::is_temporary_schema_name(&value.identity().schema);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(self.durable.budget(), &cancellation, &cancellation);
        let value = value
            .copy_definition(&control)?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("native type definitions use admitted construction"));
        let bytes = name
            .capacity()
            .checked_add(value.reserved_bytes())
            .ok_or(MemoryError::SizeOverflow)?;
        let mut durable = self.durable.clone();
        let mut session = self.temporary.clone();
        if temporary {
            durable.remove(name.as_str())?;
            session.insert(name, value, bytes)?;
        } else {
            session.remove(name.as_str())?;
            durable.insert(name, value, bytes)?;
        }
        self.durable = durable;
        self.temporary = session;
        Ok(())
    }

    /// Edit only the selected definition, keeping all other payloads shared. A failed edit or replacement leaves the original roots unchanged.
    pub fn edit<R>(
        &mut self,
        name: &str,
        edit: impl FnOnce(&mut T) -> Result<R, SQLError>,
    ) -> Result<Option<R>, SQLError> {
        let Some(mut value) = self.get(name).cloned() else {
            return Ok(None);
        };
        let result = edit(&mut value)?;
        self.insert(name.to_owned(), value)?;
        Ok(Some(result))
    }

    pub fn try_from_iter(values: impl IntoIterator<Item = (String, T)>) -> Result<Self, SQLError> {
        let mut result = Self::new();
        for (name, value) in values {
            result.insert(name, value)?;
        }
        Ok(result)
    }
}

impl<T, Q: AsRef<str> + ?Sized> std::ops::Index<&Q> for TypeDefinitions<T> {
    type Output = T;
    fn index(&self, key: &Q) -> &T {
        self.get(key.as_ref()).expect("type catalog key exists")
    }
}

/// Ordered native view over the two disjoint canonical partitions.
pub struct TypeDefinitionIter<'a, T> {
    durable: Peekable<RetainedMapIter<'a, String, Budgeted<T>>>,
    temporary: Peekable<RetainedMapIter<'a, String, Budgeted<T>>>,
}

impl<'a, T> Iterator for TypeDefinitionIter<'a, T> {
    type Item = (&'a String, &'a T);
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

impl<'a, T> IntoIterator for &'a TypeDefinitions<T> {
    type Item = (&'a String, &'a T);
    type IntoIter = TypeDefinitionIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Immutable durable definitions; the enclosing produced owner retains their independent admission.
pub struct CapturedTypeDefinitions<T> {
    root: RetainedMapSnapshot<String, Budgeted<T>>,
}

impl<T> CapturedTypeDefinitions<T> {
    pub fn len(&self) -> usize {
        self.root.len()
    }
    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }
    pub fn get(&self, name: &str) -> Option<&T> {
        self.root.get(name).map(|value| &**value)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &T)> {
        self.root.iter().map(|(name, value)| (name, &**value))
    }
}

#[cfg(test)]
mod tests;
