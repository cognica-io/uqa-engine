//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable system authority retains independently versioned relation and attribute ACLs.

use super::SystemRelationSecurity;
use crate::{
    catalog::named_definitions::{
        sealed, CapturedDefinitions, DefinitionKey, DefinitionRegistry, StoredDefinition,
    },
    SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

pub type SystemRelationSecurities = DefinitionRegistry<RelationIdentity, SystemRelationSecurity>;
pub type CapturedSystemRelationSecurities =
    CapturedDefinitions<RelationIdentity, SystemRelationSecurity>;

impl sealed::Key for RelationIdentity {}
impl DefinitionKey for RelationIdentity {
    type View = Self;
    fn payload_bytes(&self) -> usize {
        self.schema.capacity() + self.name.capacity()
    }
}

impl sealed::Definition for SystemRelationSecurity {}
impl StoredDefinition<RelationIdentity> for SystemRelationSecurity {
    fn is_temporary(&self, _: &RelationIdentity) -> bool {
        false
    }
    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        crate::plan::copy::capture_definition_with_control(self, control)
    }
}
