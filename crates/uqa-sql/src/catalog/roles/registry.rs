//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native role and membership catalogs retain immutable definitions and their complete admission footprints.

use super::{RoleDefinition, RoleMembership, RoleMembershipKey};
use crate::{
    catalog::named_definitions::{
        sealed, CapturedDefinitions, DefinitionKey, DefinitionRegistry, StoredDefinition,
    },
    plan::copy::SyntaxProduction,
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

pub type RoleDefinitions = DefinitionRegistry<String, RoleDefinition>;
pub type RoleMemberships = DefinitionRegistry<RoleMembershipKey, RoleMembership>;
pub type CapturedRoleDefinitions = CapturedDefinitions<String, RoleDefinition>;
pub type CapturedRoleMemberships = CapturedDefinitions<RoleMembershipKey, RoleMembership>;

impl RoleDefinitions {
    pub fn initial_catalog() -> Self {
        let role = RoleDefinition::bootstrap();
        Self::try_from_iter([(role.name.clone(), role)]).expect("fixed bootstrap role catalog")
    }
}

impl sealed::Key for RoleMembershipKey {}
impl DefinitionKey for RoleMembershipKey {
    type View = Self;
    fn payload_bytes(&self) -> usize {
        0
    }
}

impl sealed::Definition for RoleDefinition {}
impl StoredDefinition for RoleDefinition {
    fn is_temporary(&self, _: &str) -> bool {
        false
    }

    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        let Self {
            oid,
            object_id,
            revision,
            name,
            attributes,
            connection_limit,
        } = self;
        let output = SyntaxProduction::new(*control);
        let copied = Self {
            oid: *oid,
            object_id: *object_id,
            revision: *revision,
            name: output.text(name)?,
            attributes: output.set_copy(attributes.iter().copied())?,
            connection_limit: *connection_limit,
        };
        output.finish(copied)
    }
}

impl sealed::Definition for RoleMembership {}
impl StoredDefinition<RoleMembershipKey> for RoleMembership {
    fn is_temporary(&self, _: &RoleMembershipKey) -> bool {
        false
    }

    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        self.capture_membership(control)
    }
}

#[cfg(test)]
mod tests;
