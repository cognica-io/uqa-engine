//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validation of persisted role keys and membership identities.
use super::{identity::RoleBinding, RoleDefinition, RoleMembership};
use crate::catalog::roles::{RoleDefinitions, RoleMemberships};
use serde::{Deserialize, Serialize};
use uqa_core::memory::ProductionControl;

pub mod input;
pub mod validation;

pub fn restore_role_definitions(roles: &mut RoleDefinitions) -> Result<(), String> {
    let bootstrap = roles
        .iter()
        .any(|(name, role)| role.oid == 10 && role.name == *name);
    if needs_bootstrap(roles, bootstrap)? {
        roles
            .insert("uqa".into(), RoleDefinition::bootstrap())
            .map_err(|error| error.to_string())?;
    }
    validation::finish(roles, validation::RoleValidationMode::Definitions)
}

fn needs_bootstrap(roles: &RoleDefinitions, bootstrap: bool) -> Result<bool, String> {
    if !bootstrap {
        if roles.get("uqa").is_some_and(|role| role.oid != 10) {
            return Err("persisted bootstrap role must have OID 10".into());
        }
        return Ok(!roles.contains_key("uqa"));
    }
    Ok(false)
}

/// The former aggregate resolves names exactly once during initial-open conversion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NamedRoleMembership {
    pub oid: i64,
    pub role: String,
    pub member: String,
    pub grantor: String,
    pub admin_option: bool,
    pub inherit_option: bool,
    pub set_option: bool,
}

impl NamedRoleMembership {
    fn bind(self, roles: &RoleDefinitions) -> Result<RoleMembership, String> {
        let missing = || {
            format!(
                "persisted role membership `{}` -> `{}` has a missing role or grantor",
                self.member, self.role
            )
        };
        let target = roles.get(&self.role).ok_or_else(missing)?;
        let member = roles.get(&self.member).ok_or_else(missing)?;
        let grantor = roles.get(&self.grantor).ok_or_else(missing)?;
        let bind = |role| RoleBinding::from_definition(role).map_err(|error| error.to_string());
        Ok(RoleMembership {
            oid: self.oid,
            role: bind(target)?,
            member: bind(member)?,
            grantor: bind(grantor)?,
            admin_option: self.admin_option,
            inherit_option: self.inherit_option,
            set_option: self.set_option,
        })
    }
}

pub fn restore_named_role_memberships(
    roles: &RoleDefinitions,
    memberships: Vec<NamedRoleMembership>,
) -> Result<RoleMemberships, String> {
    restore_memberships(
        roles,
        memberships
            .into_iter()
            .map(|membership| membership.bind(roles)),
    )
}

pub fn restore_role_memberships(
    roles: &RoleDefinitions,
    memberships: Vec<RoleMembership>,
) -> Result<RoleMemberships, String> {
    restore_memberships(roles, memberships.into_iter().map(Ok))
}

fn restore_memberships(
    roles: &RoleDefinitions,
    memberships: impl Iterator<Item = Result<RoleMembership, String>>,
) -> Result<RoleMemberships, String> {
    let mut membership_map = super::RoleMemberships::new();
    let mut checks = validation::MembershipChecks::new(ProductionControl::uncontrolled());
    for role in roles.values() {
        checks.role(role).map_err(|error| error.to_string())?;
    }
    for membership in memberships {
        let membership = membership?;
        checks
            .membership(&membership)
            .map_err(|error| error.to_string())?;
        let key = membership.key();
        membership_map
            .insert(key, membership)
            .map_err(|error| error.to_string())?;
    }
    Ok(membership_map)
}

pub fn validate_role_identities(roles: &RoleDefinitions) -> Result<(), String> {
    validation::finish(roles, validation::RoleValidationMode::Identities)
}

#[cfg(test)]
mod tests;
