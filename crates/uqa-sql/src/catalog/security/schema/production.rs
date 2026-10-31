//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read persisted schema ACLs without materializing names or unowned traversal scratch.

use super::{RoleAttribute, SchemaAclPrivilege};
use crate::{
    catalog::{
        roles::{
            identity::RoleSubject, memberships::role_inherits_with_control, RoleDefinition,
            RoleIdentity, RoleMembership, RoleMembershipKey,
        },
        security::BoundSchemaSecurity,
    },
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::{
    catalog_role::BoundAclEntry, catalog_schema::SchemaPrivileges, memory::ProductionControl,
};

/// A retained registry row or a stack-backed builtin ACL has the same privilege rules.
pub struct SchemaPrivilegeView<'a> {
    pub role_owner: RoleIdentity,
    pub acl: Option<&'a [BoundAclEntry<SchemaPrivileges>]>,
}

impl<'a> From<&'a BoundSchemaSecurity> for SchemaPrivilegeView<'a> {
    fn from(security: &'a BoundSchemaSecurity) -> Self {
        Self {
            role_owner: security.role_owner,
            acl: security.acl.as_deref(),
        }
    }
}

pub fn role_has_schema_privilege_with_control(
    security: SchemaPrivilegeView<'_>,
    subject: &(impl RoleSubject + ?Sized),
    privilege: SchemaAclPrivilege,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    // Ordinary namespace lookup resolves the complete ACL before considering even PUBLIC or superuser access. Invalid persisted references therefore keep the namespace unusable.
    require_role(security.role_owner, roles, control)?;
    for entry in security.acl.into_iter().flatten() {
        control.check()?;
        if let Some(role) = entry.role {
            require_role(role, roles, control)?;
        }
        require_role(entry.grantor, roles, control)?;
    }
    if subject
        .role_definition_with_control(roles, control)?
        .is_some_and(|role| role.has(RoleAttribute::Superuser))
    {
        return Ok(true);
    }
    match security.acl {
        None => {
            role_inherits_with_control(roles, memberships, subject, &security.role_owner, control)
        }
        Some(acl) => {
            for entry in acl {
                control.check()?;
                if entry.privileges.intersects(privilege.mask())
                    && match entry.role {
                        None => true,
                        Some(role) => {
                            role_inherits_with_control(roles, memberships, subject, &role, control)?
                        }
                    }
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

fn require_role(
    role: RoleIdentity,
    roles: &BTreeMap<String, RoleDefinition>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if !role.is_valid() {
        return Err(SQLError::Internal(
            "invalid persisted schema role identity".into(),
        ));
    }
    if role.role_definition_with_control(roles, control)?.is_none() {
        return Err(SQLError::Internal(format!(
            "persisted schema privileges reference missing role incarnation {}",
            role.oid
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
