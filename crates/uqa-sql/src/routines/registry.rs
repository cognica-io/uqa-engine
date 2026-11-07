//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable routine catalogs preserve ordered overloads and independently retained bodies.

use super::{ownership, SQLRoutine};
use crate::{
    catalog::named_definitions::{
        sealed, CapturedNamedDefinitions, NamedDefinitionIter, NamedDefinitions, StoredDefinition,
    },
    plan::copy::SyntaxProduction,
    SQLError,
};
use uqa_core::{
    memory::{MemoryBudget, MemoryError, Produced, ProductionControl},
    RelationIdentity,
};

mod serialization;
#[cfg(test)]
mod tests;

#[derive(Clone, Default)]
pub struct RoutineRegistry {
    definitions: NamedDefinitions<Vec<SQLRoutine>>,
}

pub struct CapturedRoutines {
    definitions: CapturedNamedDefinitions<Vec<SQLRoutine>>,
}

impl CapturedRoutines {
    pub fn len(&self) -> usize {
        self.definitions.len()
    }
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }
    pub fn iter(
        &self,
    ) -> crate::catalog::named_definitions::CapturedDefinitionIter<'_, String, Vec<SQLRoutine>>
    {
        self.definitions.iter()
    }
}

impl<'a> IntoIterator for &'a CapturedRoutines {
    type Item = (&'a String, &'a Vec<SQLRoutine>);
    type IntoIter =
        crate::catalog::named_definitions::CapturedDefinitionIter<'a, String, Vec<SQLRoutine>>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl RoutineRegistry {
    pub(crate) fn insert_image_entry(
        &mut self,
        name: uqa_core::memory::Budgeted<String>,
        values: uqa_core::memory::Budgeted<Vec<SQLRoutine>>,
    ) -> Result<(), SQLError> {
        self.definitions.insert_image_entry(name, values)
    }

    pub(crate) fn last_durable_key(&self) -> Option<&String> {
        self.definitions.last_durable_key()
    }
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self {
            definitions: NamedDefinitions::with_budget(memory),
        }
    }
    pub fn get(&self, name: &str) -> Option<&Vec<SQLRoutine>> {
        self.definitions.get(name)
    }
    pub fn contains_key(&self, name: &str) -> bool {
        self.definitions.contains_key(name)
    }
    pub fn iter(&self) -> NamedDefinitionIter<'_, Vec<SQLRoutine>> {
        self.definitions.iter()
    }
    pub fn values(&self) -> impl Iterator<Item = &Vec<SQLRoutine>> {
        self.definitions.values()
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.definitions.keys()
    }
    pub fn len(&self) -> usize {
        self.definitions.len()
    }
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }
    pub fn clear(&mut self) {
        self.definitions.clear();
    }
    pub fn temporary_only(&self) -> Self {
        Self {
            definitions: self.definitions.temporary_only(),
        }
    }
    pub fn insert(&mut self, name: String, routines: Vec<SQLRoutine>) -> Result<(), SQLError> {
        RelationIdentity::try_borrowed_reference(&name)
            .map_err(|error| SQLError::Internal(format!("invalid stored routine name: {error}")))?;
        self.definitions.insert(name, routines)
    }
    pub fn remove(&mut self, name: &str) -> Result<bool, SQLError> {
        self.definitions.remove(name)
    }
    pub fn edit<R>(
        &mut self,
        name: &str,
        edit: impl FnOnce(&mut Vec<SQLRoutine>) -> Result<R, SQLError>,
    ) -> Result<Option<R>, SQLError> {
        self.definitions.edit(name, edit)
    }
    pub fn retain(
        &mut self,
        keep: impl FnMut(&String, &Vec<SQLRoutine>) -> bool,
    ) -> Result<(), SQLError> {
        self.definitions.retain(keep)
    }
    pub fn try_from_iter(
        values: impl IntoIterator<Item = (String, Vec<SQLRoutine>)>,
    ) -> Result<Self, SQLError> {
        let mut registry = Self::new();
        for (name, routines) in values {
            registry.insert(name, routines)?;
        }
        Ok(registry)
    }
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedRoutines>, SQLError> {
        let (definitions, memory) = self.definitions.capture(control)?.into_parts();
        Ok(control.finish(CapturedRoutines { definitions }, memory)?)
    }
}

impl sealed::Definition for Vec<SQLRoutine> {}
impl StoredDefinition for Vec<SQLRoutine> {
    fn is_temporary(&self, name: &str) -> bool {
        RelationIdentity::try_borrowed_reference(name).is_ok_and(|relation| {
            relation
                .schema
                .is_some_and(crate::catalog::temporary_namespace::is_temporary_schema_reference)
        })
    }
    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let mut routines = output.vector(self.len())?;
        for function in self {
            output.check()?;
            routines.push(if ownership::retained_by(function, control) {
                function.clone()
            } else {
                function.copy_native(control)?
            })?;
        }
        let routines = routines.finish()?;
        output.finish(routines)
    }
    fn retained_children_bytes(&self) -> Result<usize, SQLError> {
        self.iter().try_fold(0usize, |bytes, function| {
            Ok(bytes
                .checked_add(ownership::retained_bytes(function)?)
                .ok_or(MemoryError::SizeOverflow)?)
        })
    }
}

impl<'a> IntoIterator for &'a RoutineRegistry {
    type Item = (&'a String, &'a Vec<SQLRoutine>);
    type IntoIter = NamedDefinitionIter<'a, Vec<SQLRoutine>>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<Q: AsRef<str> + ?Sized> std::ops::Index<&Q> for RoutineRegistry {
    type Output = Vec<SQLRoutine>;
    fn index(&self, key: &Q) -> &Self::Output {
        &self.definitions[key.as_ref()]
    }
}
