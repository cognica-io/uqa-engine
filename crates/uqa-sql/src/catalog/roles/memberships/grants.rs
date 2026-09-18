//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Membership authorization, grantor selection and administration graph integrity.

use super::{
    insufficient_privilege, membership_error, revoke_membership, role_has_admin, role_inherits,
    role_is_superuser, role_reaches, undefined_role, RoleDefinition, RoleMembership,
    RoleMembershipKey, RoleSubject, SQLError,
};
use crate::ast::GrantRoleStmt;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

fn nearest_admin(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
    role: &str,
) -> Option<String> {
    if member == role {
        return None;
    }
    let mut queue = VecDeque::from([member.to_owned()]);
    let mut visited = BTreeSet::from([member.to_owned()]);
    while let Some(current) = queue.pop_front() {
        for membership in memberships.values().filter(|edge| edge.member == current) {
            if membership.role == role && membership.admin_option {
                return Some(current);
            }
            if membership.inherit_option && visited.insert(membership.role.clone()) {
                queue.push_back(membership.role.clone());
            }
        }
    }
    None
}

pub(super) fn select_grantor(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &(impl RoleSubject + ?Sized),
    role: &str,
    statement: &GrantRoleStmt,
) -> Result<String, SQLError> {
    let target = roles.get(role).ok_or_else(|| undefined_role(role))?;
    let action = if statement.is_grant {
        "grant"
    } else {
        "revoke"
    };
    let denied =
        || insufficient_privilege(&format!("permission denied to {action} role \"{role}\""));
    let current_name = current.role_name(roles).ok_or_else(denied)?;
    let superuser = role_is_superuser(roles, current);
    let inherited_admin = nearest_admin(memberships, current_name, role);
    if !superuser && (role_is_superuser(roles, target.name.as_str()) || inherited_admin.is_none()) {
        return Err(denied());
    }
    if let Some(grantor) = &statement.grantor {
        let may_act = role_inherits(roles, memberships, current, grantor);
        let may_grant = roles
            .get(grantor)
            .is_some_and(|definition| definition.oid == 10)
            || (grantor != role && role_has_admin(memberships, grantor, role));
        if !may_act || (statement.is_grant && !may_grant) {
            return Err(insufficient_privilege(&if statement.is_grant {
                format!("permission denied to grant privileges as role \"{grantor}\"")
            } else {
                format!("permission denied to revoke privileges granted by role \"{grantor}\"")
            }));
        }
        return Ok(grantor.clone());
    }
    if superuser {
        return roles
            .values()
            .find(|role| role.oid == 10)
            .map(|role| role.name.clone())
            .ok_or_else(|| SQLError::Internal("role catalog has no bootstrap superuser".into()));
    }
    inherited_admin.ok_or_else(denied)
}

pub(super) fn validate_grant(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    role: &str,
    grantor: &str,
    statement: &GrantRoleStmt,
) -> Result<(), SQLError> {
    for member in &statement.grantee_roles {
        if role_reaches(memberships, role, member, |_| true) {
            return Err(membership_error(format!(
                "role \"{role}\" is a member of role \"{member}\""
            )));
        }
    }
    if statement.options.admin != Some(true) || roles[grantor].oid == 10 {
        return Ok(());
    }
    let circular = || membership_error("ADMIN option cannot be granted back to your own grantor");
    if statement
        .grantee_roles
        .iter()
        .any(|member| roles[member].oid == 10)
    {
        return Err(circular());
    }
    // Remove every proposed recipient and its dependent grants together. A separate surviving ADMIN source is required for the grantor.
    let mut remaining = memberships
        .iter()
        .filter(|(_, edge)| edge.role == role)
        .map(|(key, edge)| (key.clone(), edge.clone()))
        .collect::<BTreeMap<_, _>>();
    let recipients = statement.grantee_roles.iter().collect::<BTreeSet<_>>();
    let removed = remaining
        .keys()
        .filter(|key| recipients.contains(&key.member))
        .cloned()
        .collect::<Vec<_>>();
    for key in removed {
        revoke_membership(&mut remaining, &key, true, true)?;
    }
    if !role_has_admin(&remaining, grantor, role) {
        return Err(circular());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
