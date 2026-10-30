//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation authorization observes bound ACLs without constructing a display-name copy.

use super::TableAclPrivilege;
use crate::{
    ast::RoleAttribute,
    catalog::{
        roles::{
            identity::RoleSubject, memberships::role_inherits_with_control, RoleDefinition,
            RoleIdentity, RoleMembership, RoleMembershipKey, RoleReference,
        },
        security::{
            system_relations::SystemRelationSecurity, table_binding::BoundTableAclEntry,
            BoundTableSecurity,
        },
        SystemRelation,
    },
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::memory::ProductionControl;

pub struct RelationPrivileges<'a, 'c, Subject: ?Sized = RoleReference> {
    pub subject: &'a Subject,
    pub roles: &'a BTreeMap<String, RoleDefinition>,
    pub memberships: &'a BTreeMap<RoleMembershipKey, RoleMembership>,
    pub control: ProductionControl<'c>,
}

pub type SelectPrivileges<'a, 'c> = RelationPrivileges<'a, 'c>;

impl<Subject: RoleSubject + ?Sized> RelationPrivileges<'_, '_, Subject> {
    pub fn relation(
        &self,
        security: &BoundTableSecurity,
        column: Option<&str>,
    ) -> Result<bool, SQLError> {
        self.relation_for(security, column, TableAclPrivilege::Select)
    }

    pub fn relation_for(
        &self,
        security: &BoundTableSecurity,
        column: Option<&str>,
        privilege: TableAclPrivilege,
    ) -> Result<bool, SQLError> {
        self.validate(
            security.role_owner,
            security.acl.as_deref(),
            security.column_acls.values().map(Vec::as_slice),
        )?;
        self.allowed(
            security.role_owner,
            security.acl.as_deref(),
            column.and_then(|name| security.column_acls.get(name).map(Vec::as_slice)),
            privilege,
        )
    }

    pub fn system(
        &self,
        relation: SystemRelation,
        security: Option<&SystemRelationSecurity>,
        column: Option<&str>,
    ) -> Result<bool, SQLError> {
        let (bootstrap, count) = relation.bootstrap_acl();
        let table = security
            .and_then(|security| security.table.as_ref())
            .map_or(&bootstrap[..count], |entry| entry.acl.as_slice());
        self.validate(
            RoleIdentity::BOOTSTRAP,
            Some(table),
            security
                .into_iter()
                .flat_map(|security| security.columns.values())
                .map(|entry| entry.acl.as_slice()),
        )?;
        let column = column
            .and_then(|name| security?.columns.get(name))
            .map(|entry| entry.acl.as_slice());
        self.allowed(
            RoleIdentity::BOOTSTRAP,
            Some(table),
            column,
            TableAclPrivilege::Select,
        )
    }

    pub(in crate::catalog::security) fn validate<'a>(
        &self,
        owner: RoleIdentity,
        table: Option<&'a [BoundTableAclEntry]>,
        columns: impl Iterator<Item = &'a [BoundTableAclEntry]>,
    ) -> Result<(), SQLError> {
        self.validate_role(owner)?;
        for acl in table.into_iter().chain(columns) {
            for entry in acl {
                self.control.check()?;
                if let Some(role) = entry.role {
                    self.validate_role(role)?;
                }
                self.validate_role(entry.grantor)?;
            }
        }
        Ok(())
    }

    fn validate_role(&self, identity: RoleIdentity) -> Result<(), SQLError> {
        self.control.check()?;
        if !identity.is_valid() {
            return Err(SQLError::Internal(
                "invalid persisted table role identity".into(),
            ));
        }
        if identity
            .role_definition_with_control(self.roles, &self.control)?
            .is_none()
        {
            return Err(SQLError::Internal(format!(
                "persisted table privileges reference missing role incarnation {}",
                identity.oid
            )));
        }
        Ok(())
    }

    pub(in crate::catalog::security) fn allowed(
        &self,
        owner: RoleIdentity,
        table: Option<&[BoundTableAclEntry]>,
        column: Option<&[BoundTableAclEntry]>,
        privilege: TableAclPrivilege,
    ) -> Result<bool, SQLError> {
        if self
            .subject
            .role_definition_with_control(self.roles, &self.control)?
            .is_some_and(|role| role.has(RoleAttribute::Superuser))
        {
            return Ok(true);
        }
        let table_allowed = match table {
            Some(acl) => self.acl_allowed(acl, privilege)?,
            None => role_inherits_with_control(
                self.roles,
                self.memberships,
                self.subject,
                &owner,
                &self.control,
            )?,
        };
        if table_allowed {
            return Ok(true);
        }
        column.map_or(Ok(false), |acl| self.acl_allowed(acl, privilege))
    }

    fn acl_allowed(
        &self,
        acl: &[BoundTableAclEntry],
        privilege: TableAclPrivilege,
    ) -> Result<bool, SQLError> {
        for entry in acl {
            self.control.check()?;
            if entry.privileges.intersects(privilege.mask()) {
                if let Some(role) = entry.role {
                    if role_inherits_with_control(
                        self.roles,
                        self.memberships,
                        self.subject,
                        &role,
                        &self.control,
                    )? {
                        return Ok(true);
                    }
                } else {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
