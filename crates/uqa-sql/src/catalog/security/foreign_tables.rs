//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign authority retains the owning relation's persistence and complete ACL identities.

use super::BoundTableSecurity;
use crate::{
    ast::RelationPersistence,
    catalog::named_definitions::{
        sealed, CapturedDefinitions, DefinitionRegistry, StoredDefinition,
    },
    plan::copy::capture_stored_with_control,
    SQLError,
};
use uqa_core::{
    memory::{Budgeted, MemoryBudget, Produced, ProductionControl},
    RelationIdentity,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignTableAuthority {
    persistence: RelationPersistence,
    security: BoundTableSecurity,
}

impl ForeignTableAuthority {
    pub const fn persistence(&self) -> RelationPersistence {
        self.persistence
    }

    pub fn security(&self) -> &BoundTableSecurity {
        &self.security
    }
}

impl sealed::Definition for ForeignTableAuthority {}
impl StoredDefinition<RelationIdentity> for ForeignTableAuthority {
    fn is_temporary(&self, _: &RelationIdentity) -> bool {
        self.persistence == RelationPersistence::Temporary
    }
    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        let (security, memory) = capture_stored_with_control(&self.security, control)?.into_parts();
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
pub struct ForeignTableSecurities(DefinitionRegistry<RelationIdentity, ForeignTableAuthority>);
pub type CapturedForeignTableSecurities =
    CapturedDefinitions<RelationIdentity, ForeignTableAuthority>;

impl Default for ForeignTableSecurities {
    fn default() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }
}

impl ForeignTableSecurities {
    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self(DefinitionRegistry::with_budget(memory))
    }
    /// Select the allowance for future authority changes without copying or releasing original admitted entries. Existing captured roots retain their own reservations.
    pub fn into_mutation_budget(self, memory: &MemoryBudget) -> Self {
        Self(self.0.into_mutation_budget(memory))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, relation: &RelationIdentity) -> Option<&BoundTableSecurity> {
        self.0.get(relation).map(ForeignTableAuthority::security)
    }
    pub fn contains_key(&self, relation: &RelationIdentity) -> bool {
        self.0.contains_key(relation)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&RelationIdentity, &BoundTableSecurity)> {
        self.0
            .iter()
            .map(|(relation, value)| (relation, &value.security))
    }
    pub fn entries(
        &self,
    ) -> impl Iterator<Item = (&RelationIdentity, RelationPersistence, &BoundTableSecurity)> {
        self.0
            .iter()
            .map(|(relation, value)| (relation, value.persistence, &value.security))
    }
    pub fn values(&self) -> impl Iterator<Item = &BoundTableSecurity> {
        self.iter().map(|(_, security)| security)
    }
    pub fn insert(
        &mut self,
        relation: RelationIdentity,
        persistence: RelationPersistence,
        security: BoundTableSecurity,
    ) -> Result<(), SQLError> {
        self.0.insert(
            relation,
            ForeignTableAuthority {
                persistence,
                security,
            },
        )
    }
    /// Transfer already admitted native authority into an unpublished registry without copying its maps or payloads. The paired foreign catalog owns membership and publication.
    pub fn insert_image_entry(
        &mut self,
        relation: Budgeted<RelationIdentity>,
        persistence: RelationPersistence,
        security: Budgeted<BoundTableSecurity>,
    ) -> Result<(), SQLError> {
        let (security, memory) = security.into_parts();
        self.0.insert_image_entry(
            relation,
            Budgeted::new(
                ForeignTableAuthority {
                    persistence,
                    security,
                },
                memory,
            ),
        )
    }
    pub fn remove(&mut self, relation: &RelationIdentity) -> Result<bool, SQLError> {
        self.0.remove(relation)
    }
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedForeignTableSecurities>, SQLError> {
        self.0.capture(control)
    }
}

impl std::ops::Index<&RelationIdentity> for ForeignTableSecurities {
    type Output = BoundTableSecurity;
    fn index(&self, relation: &RelationIdentity) -> &Self::Output {
        self.get(relation).expect("foreign authority exists")
    }
}
