//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Detached role catalogs retain tuple identities and the original membership grant paths.

use super::{RoleDefinition, RoleMembership, RoleMembershipKey};
use crate::{ast::RoleAttribute, plan::copy::SyntaxProduction, SQLError};
use std::collections::BTreeMap;
use uqa_core::memory::{Produced, ProductionControl};

/// Ordered attributes have an explicit admitted allocation, independent of tree-node layout.
#[derive(Debug, PartialEq, Eq)]
pub struct CapturedRoleDefinition {
    pub oid: i64,
    pub object_id: [u8; 16],
    pub revision: u64,
    pub name: String,
    pub attributes: Vec<RoleAttribute>,
    pub connection_limit: i32,
}

impl RoleDefinition {
    pub fn capture_definition(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedRoleDefinition>, SQLError> {
        let Self {
            oid,
            object_id,
            revision,
            name,
            attributes,
            connection_limit,
        } = self;
        let output = SyntaxProduction::new(*control);
        let mut copied_attributes = output.vector(attributes.len())?;
        for attribute in attributes {
            copied_attributes.push(*attribute)?;
        }
        let copied = CapturedRoleDefinition {
            oid: *oid,
            object_id: *object_id,
            revision: *revision,
            name: output.text(name)?,
            attributes: copied_attributes.finish()?,
            connection_limit: *connection_limit,
        };
        output.finish(copied)
    }
}

impl RoleMembership {
    pub fn capture_membership(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let Self {
            oid,
            role,
            member,
            grantor,
            admin_option,
            inherit_option,
            set_option,
        } = self;
        let output = SyntaxProduction::new(*control);
        let copied = Self {
            oid: *oid,
            role: output.take(role.capture_binding(control)?),
            member: output.take(member.capture_binding(control)?),
            grantor: output.take(grantor.capture_binding(control)?),
            admin_option: *admin_option,
            inherit_option: *inherit_option,
            set_option: *set_option,
        };
        output.finish(copied)
    }
}

pub fn roles(
    values: &BTreeMap<String, RoleDefinition>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(String, CapturedRoleDefinition)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut copied = output.vector(values.len())?;
    for (name, value) in values {
        let name = output.text(name)?;
        let value = output.take(value.capture_definition(control)?);
        copied.push((name, value))?;
    }
    let copied = copied.finish()?;
    output.finish(copied)
}

pub fn memberships(
    values: &BTreeMap<RoleMembershipKey, RoleMembership>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(RoleMembershipKey, RoleMembership)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut copied = output.vector(values.len())?;
    for (key, value) in values {
        let value = output.take(value.capture_membership(control)?);
        copied.push((*key, value))?;
    }
    let copied = copied.finish()?;
    output.finish(copied)
}

#[cfg(test)]
mod tests;
