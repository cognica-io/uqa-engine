//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role declaration candidates, authorization and requested-name binding.

use super::{
    guards::RoleCatalogGuards,
    memberships::{
        insufficient_privilege, require_role_attribute_authority, role_has_admin, role_is_superuser,
    },
    resolve_role_specification, RoleDefinition, RoleIdentity, RoleMembership, RoleMembershipKey,
    RoleReferenceNames,
};
use crate::catalog::roles::identity::RoleSubject;
use crate::{
    ast::{AlterRoleStmt, DropRoleStmt, RoleAttribute},
    SQLError,
};
use std::collections::{BTreeMap, BTreeSet};

pub trait RoleNotices {
    fn notice(&self, level: &str, message: &str);
}

#[derive(Clone, Copy)]
pub struct RoleValidationContext<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub notices: &'a dyn RoleNotices,
}

pub fn require_role_creation(context: &RoleValidationContext<'_>) -> Result<(), SQLError> {
    let current = context.names.current_role();
    require_createrole(&context.roles.role_definitions(), &current, "create role")
}

fn require_createrole(
    roles: &BTreeMap<String, RoleDefinition>,
    current: &(impl RoleSubject + ?Sized),
    action: &str,
) -> Result<(), SQLError> {
    let allowed = current.role_definition(roles).is_some_and(|role| {
        role.has(RoleAttribute::Superuser) || role.has(RoleAttribute::CreateRole)
    });
    if allowed {
        Ok(())
    } else {
        Err(insufficient_privilege(&format!(
            "permission denied to {action}"
        )))
    }
}

pub fn require_role_administration_for(
    catalog: &dyn RoleCatalogGuards,
    roles: &BTreeMap<String, RoleDefinition>,
    current: &(impl RoleSubject + ?Sized),
    target: &str,
    action: &str,
) -> Result<(), SQLError> {
    if role_is_superuser(roles, current) {
        return Ok(());
    }
    let can_create_roles = current
        .role_definition(roles)
        .is_some_and(|role| role.has(RoleAttribute::CreateRole));
    let memberships = catalog.role_memberships();
    if can_create_roles
        && current
            .role_definition(roles)
            .zip(roles.get(target))
            .is_some_and(|(member, role)| {
                role_has_admin(&memberships, member.identity(), role.identity())
            })
    {
        Ok(())
    } else {
        Err(insufficient_privilege(&format!(
            "permission denied to {action}"
        )))
    }
}

pub fn create_role_candidate(
    roles: &BTreeMap<String, RoleDefinition>,
    current: &(impl RoleSubject + ?Sized),
    definition: RoleDefinition,
) -> Result<(BTreeMap<String, RoleDefinition>, bool), SQLError> {
    if roles.contains_key(&definition.name) {
        return Err(SQLError::Routine {
            sqlstate: "42710".into(),
            message: format!("role \"{}\" already exists", definition.name),
        });
    }
    let current_is_superuser = role_is_superuser(roles, current);
    let mut next_roles = roles.clone();
    next_roles.insert(definition.name.clone(), definition);
    Ok((next_roles, current_is_superuser))
}

pub fn alter_role_candidate(
    context: &RoleValidationContext<'_>,
    roles: &BTreeMap<String, RoleDefinition>,
    current: &(impl RoleSubject + ?Sized),
    name: String,
    statement: &AlterRoleStmt,
) -> Result<BTreeMap<String, RoleDefinition>, SQLError> {
    let existing = roles.get(&name).cloned().ok_or_else(|| SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("role \"{name}\" does not exist"),
    })?;
    require_role_administration_for(context.roles, roles, current, &name, "alter role")?;
    require_role_attribute_authority(
        roles,
        current,
        statement.attributes.keys().copied(),
        "alter role",
    )?;
    let current_is_superuser = current
        .role_definition(roles)
        .is_some_and(|role| role.has(RoleAttribute::Superuser));
    if (statement.attributes.contains_key(&RoleAttribute::Superuser)
        || existing.has(RoleAttribute::Superuser))
        && !current_is_superuser
    {
        return Err(insufficient_privilege(
            "must be superuser to alter superuser roles or change superuser attribute",
        ));
    }
    let mut updated = existing;
    for (&attribute, &enabled) in &statement.attributes {
        if enabled {
            updated.attributes.insert(attribute);
        } else {
            updated.attributes.remove(&attribute);
        }
    }
    if let Some(value) = statement.connection_limit {
        updated.connection_limit = value;
    }
    updated.advance_revision()?;
    let mut next = roles.clone();
    next.insert(name, updated);
    Ok(next)
}

pub fn resolve_drop_role_names(
    context: &RoleValidationContext<'_>,
    statement: &DropRoleStmt,
    current: &(impl RoleSubject + ?Sized),
    session: &(impl RoleSubject + ?Sized),
    snapshot: &BTreeMap<String, RoleDefinition>,
) -> Result<Vec<String>, SQLError> {
    require_createrole(snapshot, current, "drop role")?;
    let mut names = Vec::new();
    for requested in &statement.names {
        let name = resolve_role_specification(context.names, requested).catalog_name(snapshot)?;
        if !snapshot.contains_key(&name) {
            if statement.if_exists {
                context.notices.notice(
                    "NOTICE",
                    &format!("role \"{name}\" does not exist, skipping"),
                );
                continue;
            }
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{name}\" does not exist"),
            });
        }
        require_role_drop_authority(context, snapshot, current, session, &name)?;
        names.push(name);
    }
    Ok(names)
}

pub fn require_role_drop_authority(
    context: &RoleValidationContext<'_>,
    roles: &BTreeMap<String, RoleDefinition>,
    current: &(impl RoleSubject + ?Sized),
    session: &(impl RoleSubject + ?Sized),
    name: &str,
) -> Result<(), SQLError> {
    require_createrole(roles, current, "drop role")?;
    if current.role_name(roles) == Some(name) || session.role_name(roles) == Some(name) {
        return Err(SQLError::Routine {
            sqlstate: "55006".into(),
            message: "current user cannot be dropped".into(),
        });
    }
    if roles
        .get(name)
        .is_some_and(|role| role.has(RoleAttribute::Superuser))
        && !role_is_superuser(roles, current)
    {
        return Err(insufficient_privilege("permission denied to drop role"));
    }
    require_role_administration_for(context.roles, roles, current, name, "drop role")
}

pub fn ensure_no_grantor_dependencies(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    identities: &BTreeSet<RoleIdentity>,
) -> Result<(), SQLError> {
    for membership in memberships.values() {
        if identities.contains(&membership.grantor.identity())
            && !identities.contains(&membership.role.identity())
            && !identities.contains(&membership.member.identity())
        {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{}\" cannot be dropped because some objects depend on it: privileges for membership of role {} in role {}",
                    membership.grantor.name, membership.member.name, membership.role.name
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
