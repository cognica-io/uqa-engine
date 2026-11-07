//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native event catalogs share immutable relation and declaration roots.

use super::{StoredRule, StoredTrigger};
use crate::{
    catalog::named_definitions::{
        sealed, CapturedDefinitionIter, CapturedDefinitions, DefinitionIter, DefinitionRegistry,
        StoredDefinition,
    },
    plan::copy::SyntaxProduction,
    SQLError,
};
use uqa_core::{
    memory::{
        Budgeted, MemoryBudget, Produced, ProductionControl, RetainedMap,
        RetainedMapBidirectionalIter,
    },
    CancellationToken, RelationIdentity,
};

mod rewrites;
mod serialization;
#[cfg(test)]
mod tests;

/// Exhaustive event copying belongs to SQL; declarations retain their original bound identities.
pub trait EventDefinition: sealed::Definition + Clone {
    fn copy_event(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError>;
}

pub struct EventDefinitions<T> {
    entries: RetainedMap<String, Budgeted<T>>,
}

impl<T> Clone for EventDefinitions<T> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
        }
    }
}
impl<T> Default for EventDefinitions<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T> EventDefinitions<T> {
    /// Transfer decoded payloads with their original admission into the native declaration tree.
    pub(crate) fn insert_image_entry(
        &mut self,
        name: Budgeted<String>,
        value: Budgeted<T>,
    ) -> Result<(), SQLError> {
        assert!(self.entries.budget().shares_allowance(name.budget()));
        assert!(self.entries.budget().shares_allowance(value.budget()));
        let (name, key_memory) = name.into_parts();
        let (value, mut memory) = value.into_parts();
        memory.absorb(key_memory);
        let value = Budgeted::new(value, memory);
        let bytes = value.reserved_bytes();
        self.entries.insert(name, value, bytes)?;
        Ok(())
    }

    pub(crate) fn last_key(&self) -> Option<&String> {
        self.entries
            .last_before::<str>(std::ops::Bound::Unbounded)
            .map(|(key, _)| key)
    }

    pub fn new() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }
    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self {
            entries: RetainedMap::new(memory),
        }
    }
    pub fn get(&self, name: &str) -> Option<&T> {
        self.entries.get(name).map(|value| &**value)
    }
    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn iter(&self) -> EventIter<'_, T> {
        EventIter(self.entries.iter_bidirectional())
    }
    pub fn values(&self) -> EventValues<'_, T> {
        EventValues(self.iter())
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.iter().map(|(name, _)| name)
    }
    pub fn remove(&mut self, name: &str) -> Result<bool, SQLError> {
        Ok(self.entries.remove(name)?)
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&String, &T) -> bool) -> Result<(), SQLError> {
        let mut candidate = self.clone();
        for (name, value) in self.iter() {
            if !keep(name, value) {
                candidate.remove(name)?;
            }
        }
        *self = candidate;
        Ok(())
    }
}

impl<T: EventDefinition> EventDefinitions<T> {
    pub fn insert(&mut self, name: String, value: T) -> Result<(), SQLError> {
        let memory = self.entries.budget().clone();
        let cancellation = CancellationToken::new();
        self.copy_entry(
            &name,
            &value,
            &ProductionControl::new(&memory, &cancellation, &cancellation),
        )
    }
    fn copy_entry(
        &mut self,
        name: &str,
        value: &T,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let output = SyntaxProduction::new(*control);
        let name = output.text(name)?;
        let value = output.take(value.copy_event(control)?);
        let value = output
            .finish(value)?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("native events use admitted construction"));
        let bytes = value.reserved_bytes();
        self.entries.insert(name, value, bytes)?;
        Ok(())
    }
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
    pub fn try_from_iter(entries: impl IntoIterator<Item = (String, T)>) -> Result<Self, SQLError> {
        let mut result = Self::new();
        for (name, value) in entries {
            result.insert(name, value)?;
        }
        Ok(result)
    }
}

/// Durable and temporary relation roots remain separate; selected declaration edits publish a completely admitted successor.
pub struct EventCatalog<T> {
    relations: DefinitionRegistry<RelationIdentity, EventDefinitions<T>>,
}
impl<T> Clone for EventCatalog<T> {
    fn clone(&self) -> Self {
        Self {
            relations: self.relations.clone(),
        }
    }
}
impl<T> Default for EventCatalog<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T> EventCatalog<T> {
    pub fn new() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }
    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self {
            relations: DefinitionRegistry::with_budget(memory),
        }
    }
    pub fn get(&self, relation: &RelationIdentity) -> Option<&EventDefinitions<T>> {
        self.relations.get(relation)
    }
    pub fn contains_key(&self, relation: &RelationIdentity) -> bool {
        self.relations.contains_key(relation)
    }
    pub fn iter(&self) -> DefinitionIter<'_, RelationIdentity, EventDefinitions<T>> {
        self.relations.iter()
    }
    pub fn values(&self) -> impl Iterator<Item = &EventDefinitions<T>> {
        self.relations.values()
    }
    pub fn keys(&self) -> impl Iterator<Item = &RelationIdentity> {
        self.relations.keys()
    }
    pub fn len(&self) -> usize {
        self.relations.len()
    }
    pub fn is_empty(&self) -> bool {
        self.relations.is_empty()
    }
    pub fn clear(&mut self) {
        self.relations.clear();
    }
    pub fn temporary_only(&self) -> Self {
        Self {
            relations: self.relations.temporary_only(),
        }
    }
    pub fn remove(&mut self, relation: &RelationIdentity) -> Result<bool, SQLError> {
        self.relations.remove(relation)
    }
    pub fn retain(
        &mut self,
        keep: impl FnMut(&RelationIdentity, &EventDefinitions<T>) -> bool,
    ) -> Result<(), SQLError> {
        self.relations.retain(keep)
    }
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedEventCatalog<T>>, SQLError> {
        let (relations, memory) = self.relations.capture(control)?.into_parts();
        Ok(control.finish(CapturedEventCatalog { relations }, memory)?)
    }
}
impl<T: EventDefinition> EventCatalog<T> {
    pub(crate) fn insert_image_entry(
        &mut self,
        relation: Budgeted<RelationIdentity>,
        entries: EventDefinitions<T>,
    ) -> Result<(), SQLError> {
        let memory = self.relations.budget().empty_reservation();
        self.relations
            .insert_image_entry(relation, Budgeted::new(entries, memory))
    }

    pub(crate) fn last_durable_key(&self) -> Option<&RelationIdentity> {
        self.relations.last_durable_key()
    }

    /// Transform selected declarations in a private successor. Callback or admission failure preserves every published relation root.
    pub fn rewrite(
        &mut self,
        mut rewrite: impl FnMut(&RelationIdentity, &str, &T) -> Result<Option<T>, SQLError>,
    ) -> Result<bool, SQLError> {
        let mut candidate = self.clone();
        let mut changed = false;
        for (relation, entries) in self.iter() {
            let mut next = entries.clone();
            let mut relation_changed = false;
            for (name, value) in entries {
                if let Some(value) = rewrite(relation, name, value)? {
                    next.insert(name.clone(), value)?;
                    relation_changed = true;
                }
            }
            if relation_changed {
                candidate.insert(relation.clone(), next)?;
                changed = true;
            }
        }
        *self = candidate;
        Ok(changed)
    }

    pub fn insert(
        &mut self,
        relation: RelationIdentity,
        entries: EventDefinitions<T>,
    ) -> Result<(), SQLError> {
        self.relations.insert(relation, entries)
    }
    pub fn edit<R>(
        &mut self,
        relation: &RelationIdentity,
        edit: impl FnOnce(&mut EventDefinitions<T>) -> Result<R, SQLError>,
    ) -> Result<Option<R>, SQLError> {
        self.relations.edit(relation, edit)
    }
    pub fn insert_event(
        &mut self,
        relation: RelationIdentity,
        name: String,
        value: T,
    ) -> Result<(), SQLError> {
        let mut entries = self
            .get(&relation)
            .cloned()
            .unwrap_or_else(|| EventDefinitions::with_budget(self.relations.budget()));
        entries.insert(name, value)?;
        self.insert(relation, entries)
    }
    pub fn try_from_iter(
        relations: impl IntoIterator<Item = (RelationIdentity, EventDefinitions<T>)>,
    ) -> Result<Self, SQLError> {
        let mut result = Self::new();
        for (relation, entries) in relations {
            result.insert(relation, entries)?;
        }
        Ok(result)
    }
}

impl<T> sealed::Definition for EventDefinitions<T> {}
impl<T: EventDefinition> StoredDefinition<RelationIdentity> for EventDefinitions<T> {
    fn is_temporary(&self, relation: &RelationIdentity) -> bool {
        crate::catalog::temporary_namespace::is_temporary_schema_name(&relation.schema)
    }
    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        let memory = control.budget().ok_or_else(|| {
            SQLError::Internal("native event construction requires an admitted control".into())
        })?;
        let output = SyntaxProduction::new(*control);
        output.check()?;
        let copied = if memory.shares_allowance(self.entries.budget()) {
            self.clone()
        } else {
            let mut copied = Self::with_budget(memory);
            for (name, value) in self {
                copied.copy_entry(name, value, control)?;
            }
            copied
        };
        output.finish(copied)
    }
    fn retained_children_bytes(&self) -> Result<usize, SQLError> {
        Ok(self.entries.retained_bytes()?)
    }
}
impl sealed::Definition for StoredTrigger {}
impl EventDefinition for StoredTrigger {
    fn copy_event(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        self.capture_definition(control)
    }
}
impl sealed::Definition for StoredRule {}
impl EventDefinition for StoredRule {
    fn copy_event(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let Self {
            definition,
            enabled,
            condition_plan,
            condition_binding,
            dependencies,
            catalog_oid,
        } = self;
        let copied = Self {
            definition: output.capture_stored(definition)?,
            enabled: *enabled,
            condition_plan: output.capture_stored(condition_plan)?,
            condition_binding: condition_binding
                .as_ref()
                .map(|binding| {
                    binding
                        .capture_binding(control)
                        .map(|value| output.take(value))
                })
                .transpose()?,
            dependencies: output.copy(dependencies)?,
            catalog_oid: *catalog_oid,
        };
        output.finish(copied)
    }
}

pub struct CapturedEventCatalog<T> {
    relations: CapturedDefinitions<RelationIdentity, EventDefinitions<T>>,
}
impl<T> CapturedEventCatalog<T> {
    pub fn len(&self) -> usize {
        self.relations.len()
    }
    pub fn is_empty(&self) -> bool {
        self.relations.is_empty()
    }
    pub fn get(&self, relation: &RelationIdentity) -> Option<&EventDefinitions<T>> {
        self.relations.get(relation)
    }
    pub fn iter(&self) -> CapturedDefinitionIter<'_, RelationIdentity, EventDefinitions<T>> {
        self.relations.iter()
    }
}

impl<'a, T> IntoIterator for &'a CapturedEventCatalog<T> {
    type Item = (&'a RelationIdentity, &'a EventDefinitions<T>);
    type IntoIter = CapturedDefinitionIter<'a, RelationIdentity, EventDefinitions<T>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct EventIter<'a, T>(RetainedMapBidirectionalIter<'a, String, Budgeted<T>>);
impl<'a, T> Iterator for EventIter<'a, T> {
    type Item = (&'a String, &'a T);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(name, value)| (name, &**value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl<T> DoubleEndedIterator for EventIter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(|(name, value)| (name, &**value))
    }
}
impl<T> ExactSizeIterator for EventIter<'_, T> {}
impl<T> std::iter::FusedIterator for EventIter<'_, T> {}
pub struct EventValues<'a, T>(EventIter<'a, T>);
impl<'a, T> Iterator for EventValues<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(_, value)| value)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl<T> DoubleEndedIterator for EventValues<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(|(_, value)| value)
    }
}
impl<T> ExactSizeIterator for EventValues<'_, T> {}
impl<T> std::iter::FusedIterator for EventValues<'_, T> {}
impl<'a, T> IntoIterator for &'a EventDefinitions<T> {
    type Item = (&'a String, &'a T);
    type IntoIter = EventIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<'a, T> IntoIterator for &'a EventCatalog<T> {
    type Item = (&'a RelationIdentity, &'a EventDefinitions<T>);
    type IntoIter = DefinitionIter<'a, RelationIdentity, EventDefinitions<T>>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<T> std::ops::Index<&RelationIdentity> for EventCatalog<T> {
    type Output = EventDefinitions<T>;
    fn index(&self, key: &RelationIdentity) -> &Self::Output {
        &self.relations[key]
    }
}
impl<T, Q: AsRef<str> + ?Sized> std::ops::Index<&Q> for EventDefinitions<T> {
    type Output = T;
    fn index(&self, key: &Q) -> &Self::Output {
        self.get(key.as_ref()).expect("event definition key exists")
    }
}
