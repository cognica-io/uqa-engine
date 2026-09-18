//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validation of persisted role keys and membership identities.
use super::{
    identity::{RoleBinding, RoleSubject},
    RoleDefinition, RoleMembership, RoleMembershipKey,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub fn restore_role_definitions(
    roles: &mut BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    roles
        .entry("uqa".into())
        .or_insert_with(RoleDefinition::bootstrap);
    let mut oids = BTreeSet::new();
    for (name, role) in roles.iter() {
        if role.name != *name {
            return Err(format!(
                "persisted role key `{name}` does not match role name `{}`",
                role.name
            ));
        }
        if role.oid <= 0 || role.oid > i64::from(u32::MAX) {
            return Err(format!(
                "persisted role `{name}` has an invalid OID {}",
                role.oid
            ));
        }
        if name == "uqa" && role.oid != 10 {
            return Err("persisted bootstrap role must have OID 10".into());
        }
        if !oids.insert(role.oid) {
            return Err(format!("persisted role OID {} is duplicated", role.oid));
        }
    }
    Ok(())
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
    fn bind(self, roles: &BTreeMap<String, RoleDefinition>) -> Result<RoleMembership, String> {
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
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: Vec<NamedRoleMembership>,
) -> Result<BTreeMap<RoleMembershipKey, RoleMembership>, String> {
    restore_memberships(
        roles,
        memberships
            .into_iter()
            .map(|membership| membership.bind(roles)),
    )
}

pub fn restore_role_memberships(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: Vec<RoleMembership>,
) -> Result<BTreeMap<RoleMembershipKey, RoleMembership>, String> {
    restore_memberships(roles, memberships.into_iter().map(Ok))
}

fn restore_memberships(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: impl Iterator<Item = Result<RoleMembership, String>>,
) -> Result<BTreeMap<RoleMembershipKey, RoleMembership>, String> {
    let mut membership_map = BTreeMap::new();
    let mut membership_oids = BTreeSet::new();
    let mut identities = roles
        .values()
        .map(|role| (role.object_id, role.oid))
        .collect::<BTreeMap<_, _>>();
    for membership in memberships {
        let membership = membership?;
        for binding in [&membership.role, &membership.member, &membership.grantor] {
            let oid = i64::from(binding.oid);
            if binding.oid == 0
                || binding.object_id == [0; 16]
                || (binding.oid == 10 && binding.object_id != RoleDefinition::bootstrap().object_id)
                || identities
                    .insert(binding.object_id, oid)
                    .is_some_and(|previous| previous != oid)
            {
                return Err("persisted role membership has an invalid role identity".into());
            }
        }
        if membership.grantor.role_definition(roles).is_none() {
            return Err(format!(
                "persisted role membership `{}` -> `{}` has a missing grantor identity",
                membership.member.name, membership.role.name
            ));
        }
        if membership.oid <= 0 || membership.oid > i64::from(u32::MAX) {
            return Err(format!(
                "persisted role membership has an invalid OID {}",
                membership.oid
            ));
        }
        if !membership_oids.insert(membership.oid) {
            return Err(format!(
                "persisted role membership OID {} is duplicated",
                membership.oid
            ));
        }
        let key = membership.key();
        if membership_map.insert(key, membership).is_some() {
            return Err("persisted role membership identity is duplicated".into());
        }
    }
    Ok(membership_map)
}

pub fn validate_role_identities(roles: &BTreeMap<String, RoleDefinition>) -> Result<(), String> {
    let mut identities = BTreeSet::new();
    for (name, role) in roles {
        if role.object_id == [0; 16] {
            return Err(format!("persisted role `{name}` has no object identity"));
        }
        if name == "uqa" && role.object_id != RoleDefinition::bootstrap().object_id {
            return Err("persisted bootstrap role has an invalid object identity".into());
        }
        if !identities.insert(role.object_id) {
            return Err("persisted role object identity is duplicated".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
