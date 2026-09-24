//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Membership authorization, grantor selection and administration graph integrity.

use super::{
    insufficient_privilege, membership_error, revoke_membership, role_has_admin, role_is_superuser,
    role_reaches, RoleBinding, RoleDefinition, RoleIdentity, RoleMembership, RoleMembershipKey,
    RoleSubject, SQLError,
};
use crate::ast::RoleMembershipOptions;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

fn nearest_admin(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: RoleIdentity,
    role: RoleIdentity,
) -> Option<RoleBinding> {
    if member == role {
        return None;
    }
    let mut queue = VecDeque::from([member]);
    let mut visited = BTreeSet::from([member]);
    while let Some(current) = queue.pop_front() {
        for membership in memberships
            .values()
            .filter(|edge| edge.member.identity() == current)
        {
            if membership.role.identity() == role && membership.admin_option {
                return Some(membership.member.clone());
            }
            if membership.inherit_option && visited.insert(membership.role.identity()) {
                queue.push_back(membership.role.identity());
            }
        }
    }
    None
}

pub(super) fn require_group_authority(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &(impl RoleSubject + ?Sized),
    target: &RoleBinding,
) -> Result<(), SQLError> {
    if role_is_superuser(roles, current)
        || (!role_is_superuser(roles, target)
            && current.role_definition(roles).is_some_and(|role| {
                nearest_admin(memberships, role.identity(), target.identity()).is_some()
            }))
    {
        Ok(())
    } else {
        Err(insufficient_privilege("permission denied to alter role"))
    }
}

pub(super) fn select_grantor(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &(impl RoleSubject + ?Sized),
    target: &RoleBinding,
    requested_grantor: Option<&RoleBinding>,
    is_grant: bool,
) -> Result<RoleBinding, SQLError> {
    let role = &target.name;
    let action = if is_grant { "grant" } else { "revoke" };
    let denied =
        || insufficient_privilege(&format!("permission denied to {action} role \"{role}\""));
    let current_role = current.role_definition(roles).ok_or_else(denied)?;
    let superuser = role_is_superuser(roles, current);
    let inherited_admin = nearest_admin(memberships, current_role.identity(), target.identity());
    if !superuser && (role_is_superuser(roles, target) || inherited_admin.is_none()) {
        return Err(denied());
    }
    if let Some(grantor) = requested_grantor {
        let may_act = superuser
            || role_reaches(
                memberships,
                current_role.identity(),
                grantor.identity(),
                |edge| edge.inherit_option,
            );
        let may_grant = grantor.oid == 10
            || (grantor.identity() != target.identity()
                && role_has_admin(memberships, grantor.identity(), target.identity()));
        let name = &grantor.name;
        if !may_act || (is_grant && !may_grant) {
            return Err(insufficient_privilege(&if is_grant {
                format!("permission denied to grant privileges as role \"{name}\"")
            } else {
                format!("permission denied to revoke privileges granted by role \"{name}\"")
            }));
        }
        return Ok(grantor.clone());
    }
    if superuser {
        return roles
            .values()
            .find(|role| role.oid == 10)
            .ok_or_else(|| SQLError::Internal("role catalog has no bootstrap superuser".into()))
            .and_then(RoleBinding::from_definition);
    }
    inherited_admin.ok_or_else(denied)
}

pub(super) fn validate_grant(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    role: &RoleBinding,
    grantor: &RoleBinding,
    members: &[RoleBinding],
    options: RoleMembershipOptions,
) -> Result<(), SQLError> {
    for member in members {
        if role_reaches(memberships, role.identity(), member.identity(), |_| true) {
            return Err(membership_error(format!(
                "role \"{}\" is a member of role \"{}\"",
                role.name, member.name
            )));
        }
    }
    if options.admin != Some(true) || grantor.oid == 10 {
        return Ok(());
    }
    let circular = || membership_error("ADMIN option cannot be granted back to your own grantor");
    if members.iter().any(|member| member.oid == 10) {
        return Err(circular());
    }
    // Remove every proposed recipient and its dependent grants together. A separate surviving ADMIN source is required for the grantor.
    let mut remaining = memberships
        .iter()
        .filter(|(_, edge)| edge.role.identity() == role.identity())
        .map(|(key, edge)| (*key, edge.clone()))
        .collect::<BTreeMap<_, _>>();
    let recipients = members
        .iter()
        .map(RoleBinding::identity)
        .collect::<BTreeSet<_>>();
    let removed = remaining
        .keys()
        .filter(|key| recipients.contains(&key.member))
        .copied()
        .collect::<Vec<_>>();
    for key in removed {
        revoke_membership(&mut remaining, &key, true, true)?;
    }
    if !role_has_admin(&remaining, grantor.identity(), role.identity()) {
        return Err(circular());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
