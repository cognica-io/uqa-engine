//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read persisted schema ACLs without materializing names or unowned traversal scratch.

use super::{RoleAttribute, SchemaAclPrivilege};
use crate::catalog::roles::{RoleDefinitions, RoleMemberships};
use crate::{
    catalog::{
        roles::{
            identity::{find_identity, RoleSubject},
            memberships::{role_inherits_with_control, role_reaches_from},
            RoleIdentity, RoleReference,
        },
        security::BoundSchemaSecurity,
    },
    SQLError,
};
use uqa_core::{
    catalog_role::BoundAclEntry, catalog_schema::SchemaPrivileges, memory::ProductionControl,
};

/// A retained registry row or a stack-backed builtin ACL has the same privilege rules.
#[derive(Clone, Copy)]
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
    roles: &RoleDefinitions,
    memberships: &RoleMemberships,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    crate::catalog::work::inline(validate(security, roles, control, &mut ()))?;
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

/// Check one native session subject under retained work, after validating the complete persisted ACL in its original order.
pub async fn role_has_schema_privilege_from_input(
    security: SchemaPrivilegeView<'_>,
    subject: &RoleReference,
    privilege: SchemaAclPrivilege,
    roles: &RoleDefinitions,
    memberships: &RoleMemberships,
    control: &ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> Result<bool, SQLError> {
    validate(security, roles, control, input).await?;
    let subject = subject
        .role_definition_from_input(roles, control, input)
        .await?;
    allowed(security, subject, privilege, memberships, control, input).await
}

async fn validate(
    security: SchemaPrivilegeView<'_>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl crate::catalog::work::Work,
) -> Result<(), SQLError> {
    // Ordinary namespace lookup resolves every ACL reference before considering PUBLIC or superuser access.
    require_role(security.role_owner, roles, control, work).await?;
    for entry in security.acl.into_iter().flatten() {
        work.checkpoint().await?;
        control.check()?;
        if let Some(role) = entry.role {
            require_role(role, roles, control, work).await?;
        }
        require_role(entry.grantor, roles, control, work).await?;
    }
    Ok(())
}

async fn allowed(
    security: SchemaPrivilegeView<'_>,
    subject: Option<&crate::catalog::roles::RoleDefinition>,
    privilege: SchemaAclPrivilege,
    memberships: &RoleMemberships,
    control: &ProductionControl<'_>,
    work: &mut impl crate::catalog::work::Work,
) -> Result<bool, SQLError> {
    if subject.is_some_and(|role| role.has(RoleAttribute::Superuser)) {
        return Ok(true);
    }
    let subject = subject.map(crate::catalog::roles::RoleDefinition::identity);
    match security.acl {
        None => match subject {
            Some(subject) => {
                role_reaches_from(memberships, subject, security.role_owner, control, work).await
            }
            None => Ok(false),
        },
        Some(acl) => {
            for entry in acl {
                work.checkpoint().await?;
                control.check()?;
                if entry.privileges.intersects(privilege.mask())
                    && match (entry.role, subject) {
                        (None, _) => true,
                        (Some(role), Some(subject)) => {
                            role_reaches_from(memberships, subject, role, control, work).await?
                        }
                        _ => false,
                    }
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

async fn require_role(
    role: RoleIdentity,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl crate::catalog::work::Work,
) -> Result<(), SQLError> {
    control.check()?;
    if !role.is_valid() {
        return Err(SQLError::Internal(
            "invalid persisted schema role identity".into(),
        ));
    }
    if find_identity(role, roles, control, work).await?.is_none() {
        return Err(SQLError::Internal(format!(
            "persisted schema privileges reference missing role incarnation {}",
            role.oid
        )));
    }
    Ok(())
}

#[cfg(test)]
mod input_tests;
#[cfg(test)]
mod tests;
