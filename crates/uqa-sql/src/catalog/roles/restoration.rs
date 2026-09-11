//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validation of persisted role keys and membership identities.
use super::{RoleDefinition, RoleMembership, RoleMembershipKey};
use std::collections::{BTreeMap, BTreeSet};

pub fn restore_role_definitions(
    roles: &mut BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    roles
        .entry("uqa".into())
        .or_insert_with(RoleDefinition::bootstrap);
    for (name, role) in roles.iter() {
        if role.name != *name {
            return Err(format!(
                "persisted role key `{name}` does not match role name `{}`",
                role.name
            ));
        }
    }
    Ok(())
}

pub fn restore_role_memberships(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: Vec<RoleMembership>,
) -> Result<BTreeMap<RoleMembershipKey, RoleMembership>, String> {
    let mut membership_map = BTreeMap::new();
    let mut membership_oids = BTreeSet::new();
    for membership in memberships {
        if !roles.contains_key(&membership.role)
            || !roles.contains_key(&membership.member)
            || !roles.contains_key(&membership.grantor)
        {
            return Err(format!(
                "persisted role membership `{}` -> `{}` has a missing role or grantor",
                membership.member, membership.role
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

#[cfg(test)]
mod tests;
