//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role declaration candidates, authorization and requested-name binding.

use super::{
    guards::RoleCatalogGuards,
    memberships::{
        apply_grant_role_statement, insert_membership, insufficient_privilege,
        require_role_attribute_authority, role_has_admin, role_is_superuser,
    },
    resolve_role_reference, RoleDefinition, RoleMembership, RoleMembershipKey, RoleReferenceNames,
};
use crate::{
    ast::{
        AlterRoleStmt, CreateRoleStmt, DropRoleStmt, GrantRoleStmt, RoleAttribute,
        RoleMembershipOptions,
    },
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
    let current = context.names.current_user_name();
    let allowed = context
        .roles
        .role_definitions()
        .get(&current)
        .is_some_and(|role| {
            role.has(RoleAttribute::Superuser) || role.has(RoleAttribute::CreateRole)
        });
    if allowed {
        Ok(())
    } else {
        Err(insufficient_privilege("permission denied to create role"))
    }
}

pub fn require_role_administration_for(
    catalog: &dyn RoleCatalogGuards,
    roles: &BTreeMap<String, RoleDefinition>,
    current: &str,
    target: &str,
    action: &str,
) -> Result<(), SQLError> {
    if role_is_superuser(roles, current) {
        return Ok(());
    }
    let can_create_roles = roles
        .get(current)
        .is_some_and(|role| role.has(RoleAttribute::CreateRole));
    let memberships = catalog.role_memberships();
    if can_create_roles && role_has_admin(&memberships, current, target) {
        Ok(())
    } else {
        Err(insufficient_privilege(&format!(
            "permission denied to {action}"
        )))
    }
}

pub fn create_role_candidate(
    roles: &BTreeMap<String, RoleDefinition>,
    current: &str,
    statement: &CreateRoleStmt,
) -> Result<(BTreeMap<String, RoleDefinition>, bool), SQLError> {
    if roles.contains_key(&statement.name) {
        return Err(SQLError::Routine {
            sqlstate: "42710".into(),
            message: format!("role \"{}\" already exists", statement.name),
        });
    }
    let current_is_superuser = role_is_superuser(roles, current);
    let mut next_roles = roles.clone();
    next_roles.insert(
        statement.name.clone(),
        RoleDefinition::from_create(statement),
    );
    Ok((next_roles, current_is_superuser))
}

pub fn apply_create_role_memberships(
    context: &RoleValidationContext<'_>,
    statement: &CreateRoleStmt,
    current: &str,
    current_is_superuser: bool,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Result<(), SQLError> {
    if !current_is_superuser {
        let bootstrap = roles
            .values()
            .find(|role| role.has(RoleAttribute::Superuser))
            .map(|role| role.name.clone())
            .ok_or_else(|| SQLError::Internal("role catalog has no bootstrap superuser".into()))?;
        insert_membership(
            memberships,
            &statement.name,
            current,
            &bootstrap,
            RoleMembershipOptions {
                admin: Some(true),
                inherit: Some(false),
                set: Some(false),
            },
            roles,
        );
    }
    let in_roles = statement
        .in_roles
        .iter()
        .map(|role| resolve_role_reference(context.names, role))
        .collect::<Vec<_>>();
    if !in_roles.is_empty() {
        apply_grant_role_statement(
            roles,
            memberships,
            current,
            &GrantRoleStmt {
                granted_roles: in_roles,
                grantee_roles: vec![statement.name.clone()],
                is_grant: true,
                options: RoleMembershipOptions::default(),
                grantor: None,
                cascade: false,
            },
        )?;
    }
    let role_members = statement
        .role_members
        .iter()
        .map(|role| resolve_role_reference(context.names, role))
        .collect::<Vec<_>>();
    if !role_members.is_empty() {
        apply_grant_role_statement(
            roles,
            memberships,
            current,
            &GrantRoleStmt {
                granted_roles: vec![statement.name.clone()],
                grantee_roles: role_members,
                is_grant: true,
                options: RoleMembershipOptions::default(),
                grantor: None,
                cascade: false,
            },
        )?;
    }
    let admin_members = statement
        .admin_members
        .iter()
        .map(|role| resolve_role_reference(context.names, role))
        .collect::<Vec<_>>();
    if !admin_members.is_empty() {
        apply_grant_role_statement(
            roles,
            memberships,
            current,
            &GrantRoleStmt {
                granted_roles: vec![statement.name.clone()],
                grantee_roles: admin_members,
                is_grant: true,
                options: RoleMembershipOptions {
                    admin: Some(true),
                    ..RoleMembershipOptions::default()
                },
                grantor: None,
                cascade: false,
            },
        )?;
    }
    Ok(())
}

pub fn alter_role_candidate(
    context: &RoleValidationContext<'_>,
    roles: &BTreeMap<String, RoleDefinition>,
    current: &str,
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
    let current_is_superuser = roles
        .get(current)
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
    let mut next = roles.clone();
    next.insert(name, updated);
    Ok(next)
}

pub fn resolve_drop_role_names(
    context: &RoleValidationContext<'_>,
    statement: &DropRoleStmt,
    current: &str,
    session: &str,
    snapshot: &BTreeMap<String, RoleDefinition>,
) -> Result<Vec<String>, SQLError> {
    let mut names = Vec::new();
    for requested in &statement.names {
        let name = resolve_role_reference(context.names, requested);
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
        if name == current || name == session {
            return Err(SQLError::Routine {
                sqlstate: "55006".into(),
                message: "current user cannot be dropped".into(),
            });
        }
        require_role_administration_for(context.roles, snapshot, current, &name, "drop role")?;
        names.push(name);
    }
    Ok(names)
}

pub fn bind_grant_role_statement(
    context: &RoleValidationContext<'_>,
    statement: &GrantRoleStmt,
) -> GrantRoleStmt {
    GrantRoleStmt {
        granted_roles: statement
            .granted_roles
            .iter()
            .map(|role| resolve_role_reference(context.names, role))
            .collect(),
        grantee_roles: statement
            .grantee_roles
            .iter()
            .map(|role| resolve_role_reference(context.names, role))
            .collect(),
        is_grant: statement.is_grant,
        options: statement.options,
        grantor: statement
            .grantor
            .as_ref()
            .map(|role| resolve_role_reference(context.names, role)),
        cascade: statement.cascade,
    }
}

pub fn ensure_no_grantor_dependencies(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    names_set: &BTreeSet<String>,
) -> Result<(), SQLError> {
    for membership in memberships.values() {
        if names_set.contains(&membership.grantor)
            && !names_set.contains(&membership.role)
            && !names_set.contains(&membership.member)
        {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{}\" cannot be dropped because some objects depend on it: privileges for membership of role {} in role {}",
                    membership.grantor, membership.member, membership.role
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
