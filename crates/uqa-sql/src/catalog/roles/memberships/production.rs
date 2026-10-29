//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role reachability keeps the queue and visited identities in the invoking allowance.

use super::{RoleAttribute, RoleDefinition, RoleIdentity, RoleMembership, RoleMembershipKey};
use crate::{catalog::roles::identity::RoleSubject, SQLError};
use std::collections::BTreeMap;
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};

pub fn role_inherits_with_control(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &(impl RoleSubject + ?Sized),
    role: &(impl RoleSubject + ?Sized),
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let Some(member) = member.role_definition_with_control(roles, control)? else {
        return Ok(false);
    };
    if member.has(RoleAttribute::Superuser) {
        return Ok(true);
    }
    let Some(role) = role.role_definition_with_control(roles, control)? else {
        return Ok(false);
    };
    role_reaches_with_control(memberships, member.identity(), role.identity(), control)
}

fn role_reaches_with_control(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: RoleIdentity,
    role: RoleIdentity,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if member == role {
        return Ok(true);
    }
    let mut queue = ProductionVec::new(*control);
    let mut visited = ProductionMap::new(*control);
    queue.push_copy(member)?;
    visited.insert_copy(member, ())?;
    let mut head = 0;
    while let Some(&current) = queue.get(head) {
        head += 1;
        for membership in memberships.values() {
            control.check()?;
            if membership.member.identity() != current || !membership.inherit_option {
                continue;
            }
            let inherited = membership.role.identity();
            if inherited == role {
                return Ok(true);
            }
            if visited.insert_copy(inherited, ())?.is_none() {
                queue.push_copy(inherited)?;
            }
        }
    }
    control.check()?;
    Ok(false)
}

#[cfg(test)]
mod tests;
