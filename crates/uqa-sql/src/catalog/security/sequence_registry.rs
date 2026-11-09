//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence authority retains complete bound grants and its owning object's persistence.

use super::BoundSequenceSecurity;
use crate::{
    ast::RelationPersistence,
    catalog::named_definitions::{
        sealed, CapturedDefinitions, DefinitionRegistry, StoredDefinition,
    },
    SQLError,
};
use uqa_core::{
    memory::{Budgeted, MemoryBudget, Produced, ProductionControl},
    RelationIdentity,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceAuthority {
    persistence: RelationPersistence,
    security: BoundSequenceSecurity,
}

impl SequenceAuthority {
    pub const fn persistence(&self) -> RelationPersistence {
        self.persistence
    }

    pub fn security(&self) -> &BoundSequenceSecurity {
        &self.security
    }
}

impl sealed::Definition for SequenceAuthority {}
impl StoredDefinition<RelationIdentity> for SequenceAuthority {
    fn is_temporary(&self, _: &RelationIdentity) -> bool {
        self.persistence == RelationPersistence::Temporary
    }

    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        let (security, memory) = self.security.capture_security(control)?.into_parts();
        Ok(control.finish(
            Self {
                persistence: self.persistence,
                security,
            },
            memory,
        )?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceSecurities(DefinitionRegistry<RelationIdentity, SequenceAuthority>);
pub type CapturedSequenceSecurities = CapturedDefinitions<RelationIdentity, SequenceAuthority>;

impl Default for SequenceSecurities {
    fn default() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }
}

impl SequenceSecurities {
    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self(DefinitionRegistry::with_budget(memory))
    }

    pub(crate) fn into_mutation_budget(self, memory: &MemoryBudget) -> Self {
        Self(self.0.into_mutation_budget(memory))
    }
    pub fn shares_roots(&self, other: &Self) -> bool {
        self.0.shares_roots(&other.0)
    }
    pub fn budget(&self) -> &MemoryBudget {
        self.0.budget()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, relation: &RelationIdentity) -> Option<&BoundSequenceSecurity> {
        self.0.get(relation).map(SequenceAuthority::security)
    }
    pub fn persistence(&self, relation: &RelationIdentity) -> Option<RelationPersistence> {
        self.0.get(relation).map(SequenceAuthority::persistence)
    }

    pub(crate) fn last_durable_key(&self) -> Option<&RelationIdentity> {
        self.0.last_durable_key()
    }

    pub(crate) fn insert_image_entry(
        &mut self,
        relation: Budgeted<RelationIdentity>,
        persistence: RelationPersistence,
        security: Budgeted<BoundSequenceSecurity>,
    ) -> Result<(), SQLError> {
        let (security, memory) = security.into_parts();
        self.0.insert_image_entry(
            relation,
            Budgeted::new(
                SequenceAuthority {
                    persistence,
                    security,
                },
                memory,
            ),
        )
    }
    pub fn contains_key(&self, relation: &RelationIdentity) -> bool {
        self.0.contains_key(relation)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&RelationIdentity, &BoundSequenceSecurity)> {
        self.0
            .iter()
            .map(|(relation, value)| (relation, &value.security))
    }
    pub fn values(&self) -> impl Iterator<Item = &BoundSequenceSecurity> {
        self.iter().map(|(_, security)| security)
    }
    pub fn entries(
        &self,
    ) -> impl Iterator<
        Item = (
            &RelationIdentity,
            RelationPersistence,
            &BoundSequenceSecurity,
        ),
    > {
        self.0
            .iter()
            .map(|(relation, value)| (relation, value.persistence, &value.security))
    }
    pub fn temporary_only(&self) -> Self {
        Self(self.0.temporary_only())
    }
    pub fn edit<R>(
        &mut self,
        relation: &RelationIdentity,
        edit: impl FnOnce(&mut BoundSequenceSecurity) -> Result<R, SQLError>,
    ) -> Result<Option<R>, SQLError> {
        self.0.edit(relation, |entry| edit(&mut entry.security))
    }
    pub fn try_from_iter(
        entries: impl IntoIterator<
            Item = (RelationIdentity, RelationPersistence, BoundSequenceSecurity),
        >,
    ) -> Result<Self, SQLError> {
        Ok(Self(DefinitionRegistry::try_from_iter(
            entries.into_iter().map(|(name, persistence, security)| {
                (
                    name,
                    SequenceAuthority {
                        persistence,
                        security,
                    },
                )
            }),
        )?))
    }
    pub fn insert(
        &mut self,
        relation: RelationIdentity,
        persistence: RelationPersistence,
        security: BoundSequenceSecurity,
    ) -> Result<(), SQLError> {
        self.0.insert(
            relation,
            SequenceAuthority {
                persistence,
                security,
            },
        )
    }
    pub fn remove(&mut self, relation: &RelationIdentity) -> Result<bool, SQLError> {
        self.0.remove(relation)
    }
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedSequenceSecurities>, SQLError> {
        self.0.capture(control)
    }
}

impl std::ops::Index<&RelationIdentity> for SequenceSecurities {
    type Output = BoundSequenceSecurity;
    fn index(&self, relation: &RelationIdentity) -> &Self::Output {
        self.get(relation).expect("sequence authority exists")
    }
}
