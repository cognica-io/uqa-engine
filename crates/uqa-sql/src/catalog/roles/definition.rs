//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role declaration candidates, authorization and requested-name binding.

use super::{
    guards::RoleCatalogGuards,
    memberships::{
        insufficient_privilege, require_role_attribute_authority, role_has_transitive_admin,
        role_is_superuser,
    },
    RoleDefinition, RoleIdentity, RoleMembership, RoleMembershipKey, RoleReference,
    RoleReferenceNames,
};
use crate::catalog::roles::identity::{RoleBinding, RoleSubject};
use crate::{
    ast::{AlterRoleStmt, RoleAttribute, RoleSpecification},
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
                role_has_transitive_admin(&memberships, member.identity(), role.identity())
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

/// Initial CREATEROLE authority is checked once; each later target uses the current membership graph after preceding removals.
pub struct RoleDropAuthority {
    current: RoleBinding,
    session: RoleReference,
}

impl RoleDropAuthority {
    pub fn new(
        current: RoleReference,
        session: RoleReference,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<Self, SQLError> {
        require_createrole(roles, &current, "drop role")?;
        let session = match session {
            RoleReference::Bound(_) => session,
            RoleReference::Named(_) => {
                RoleReference::Bound(std::sync::Arc::new(session.bind(roles)?))
            }
        };
        Ok(Self {
            current: current.bind(roles)?,
            session,
        })
    }

    /// Resolve only the next target. Execution must finish its object wait and remove its memberships before requesting another target.
    pub fn resolve_target(
        &self,
        context: &RoleValidationContext<'_>,
        requested: &RoleSpecification,
        if_exists: bool,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<Option<RoleBinding>, SQLError> {
        // PostgreSQL treats the exact lowercase name public as a role specifier even when quoted.
        let name = match requested {
            RoleSpecification::Named(name) if name != "public" => name,
            _ => {
                return Err(SQLError::Routine {
                    sqlstate: "22023".into(),
                    message: "cannot use special role specifier in DROP ROLE".into(),
                });
            }
        };
        let Some(role) = roles.get(name) else {
            if if_exists {
                context.notices.notice(
                    "NOTICE",
                    &format!("role \"{name}\" does not exist, skipping"),
                );
                return Ok(None);
            }
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{name}\" does not exist"),
            });
        };
        require_role_drop_authority(context, roles, &self.current, &self.session, name)?;
        RoleBinding::from_definition(role).map(Some)
    }
}

pub fn role_drop_memberships_candidate(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    removed: RoleIdentity,
) -> BTreeMap<RoleMembershipKey, RoleMembership> {
    memberships
        .iter()
        .filter(|(_, membership)| {
            membership.role.identity() != removed && membership.member.identity() != removed
        })
        .map(|(key, membership)| (*key, membership.clone()))
        .collect()
}

fn require_role_drop_authority(
    context: &RoleValidationContext<'_>,
    roles: &BTreeMap<String, RoleDefinition>,
    current: &(impl RoleSubject + ?Sized),
    session: &(impl RoleSubject + ?Sized),
    name: &str,
) -> Result<(), SQLError> {
    let protected_user = if current.role_name(roles) == Some(name)
        || context.names.outer_role().role_name(roles) == Some(name)
    {
        Some("current")
    } else if session.role_name(roles) == Some(name) {
        Some("session")
    } else {
        None
    };
    if let Some(subject) = protected_user {
        return Err(SQLError::Routine {
            sqlstate: "55006".into(),
            message: format!("{subject} user cannot be dropped"),
        });
    }
    if roles
        .get(name)
        .is_some_and(|role| role.has(RoleAttribute::Superuser))
        && !role_is_superuser(roles, current)
    {
        return Err(insufficient_privilege("permission denied to drop role"));
    }
    if role_is_superuser(roles, current) {
        return Ok(());
    }
    let memberships = context.roles.role_memberships();
    if current
        .role_definition(roles)
        .zip(roles.get(name))
        .is_some_and(|(member, role)| {
            role_has_transitive_admin(&memberships, member.identity(), role.identity())
        })
    {
        Ok(())
    } else {
        Err(insufficient_privilege("permission denied to drop role"))
    }
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
