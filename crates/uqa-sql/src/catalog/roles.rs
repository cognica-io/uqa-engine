//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role definitions and membership semantics over explicit catalog values.

use crate::ast::{CreateRoleStmt, RoleAttribute};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub mod identity;
pub mod rename;
pub mod session;
pub mod tuple;
use identity::RoleBinding;
pub use identity::{RoleIdentity, RoleReference};
pub mod memberships;
pub use memberships::{role_can_set, role_inherits};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleDefinition {
    pub oid: i64,
    /// Durable incarnation independent of the recyclable SQL-visible OID. Zero identifies legacy metadata that requires initial-open migration.
    #[serde(default)]
    pub object_id: [u8; 16],
    /// Version of this definition tuple; even an attribute assignment of the same value creates a new tuple version. Zero requires initial-open conversion.
    #[serde(default)]
    pub revision: u64,
    pub name: String,
    pub attributes: BTreeSet<RoleAttribute>,
    pub connection_limit: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoleMembershipKey {
    pub role: RoleIdentity,
    pub member: RoleIdentity,
    pub grantor: RoleIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleMembership {
    pub oid: i64,
    pub role: RoleBinding,
    pub member: RoleBinding,
    pub grantor: RoleBinding,
    pub admin_option: bool,
    pub inherit_option: bool,
    pub set_option: bool,
}

impl RoleMembership {
    pub fn key(&self) -> RoleMembershipKey {
        RoleMembershipKey {
            role: self.role.identity(),
            member: self.member.identity(),
            grantor: self.grantor.identity(),
        }
    }
}

impl RoleDefinition {
    pub fn identity(&self) -> RoleIdentity {
        RoleIdentity {
            oid: self.oid,
            object_id: self.object_id,
        }
    }

    pub fn bootstrap() -> Self {
        Self {
            oid: 10,
            object_id: RoleIdentity::BOOTSTRAP.object_id,
            revision: 1,
            name: "uqa".into(),
            attributes: BTreeSet::from([
                RoleAttribute::Superuser,
                RoleAttribute::Inherit,
                RoleAttribute::CreateRole,
                RoleAttribute::CreateDb,
                RoleAttribute::Login,
                RoleAttribute::BypassRls,
            ]),
            connection_limit: -1,
        }
    }

    pub fn from_create(statement: &CreateRoleStmt, oid: i64, object_id: [u8; 16]) -> Self {
        Self {
            oid,
            object_id,
            revision: 1,
            name: statement.name.clone(),
            attributes: statement.attributes.clone(),
            connection_limit: statement.connection_limit,
        }
    }

    pub fn has(&self, attribute: RoleAttribute) -> bool {
        self.attributes.contains(&attribute)
    }

    pub fn advance_revision(&mut self) -> Result<(), crate::SQLError> {
        self.revision = self
            .revision
            .checked_add(1)
            .filter(|_| self.revision != 0)
            .ok_or_else(|| {
                crate::SQLError::Internal("invalid or exhausted role tuple revision".into())
            })?;
        Ok(())
    }
}

/// Selected identities used by SQL current-user, session-user and authenticated-role references.
pub trait RoleReferenceNames {
    fn current_role(&self) -> RoleReference;
    fn session_role(&self) -> RoleReference;
    /// Session-selected role before any SECURITY DEFINER substitution.
    fn outer_role(&self) -> RoleReference;
    fn authenticated_role(&self) -> RoleReference {
        self.session_role()
    }
}
pub fn resolve_role_specification(
    names: &dyn RoleReferenceNames,
    specification: &crate::ast::RoleSpecification,
) -> RoleReference {
    match specification {
        crate::ast::RoleSpecification::Named(name) => name.clone().into(),
        crate::ast::RoleSpecification::CurrentUser => names.current_role(),
        crate::ast::RoleSpecification::SessionUser => names.session_role(),
    }
}

pub fn resolve_acl_role_specification(
    names: &dyn RoleReferenceNames,
    specification: &crate::ast::AclRoleSpecification,
    roles: &std::collections::BTreeMap<String, RoleDefinition>,
) -> Result<uqa_core::catalog_acl::AclGrantee, crate::SQLError> {
    use crate::ast::AclRoleSpecification;
    use uqa_core::catalog_acl::AclGrantee;
    match specification {
        AclRoleSpecification::Public => Ok(AclGrantee::Public),
        AclRoleSpecification::Role(role) => resolve_role_specification(names, role)
            .catalog_name(roles)
            .map(AclGrantee::Role),
    }
}

pub fn require_role_exists(
    roles: &std::collections::BTreeMap<String, RoleDefinition>,
    name: &str,
) -> Result<(), crate::SQLError> {
    if roles.contains_key(name) {
        return Ok(());
    }
    Err(crate::SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("role \"{name}\" does not exist"),
    })
}
pub fn require_set_role(
    roles: &std::collections::BTreeMap<String, RoleDefinition>,
    memberships: &std::collections::BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &(impl identity::RoleSubject + ?Sized),
    target: &str,
) -> Result<(), crate::SQLError> {
    if role_can_set(roles, memberships, current, target) {
        return Ok(());
    }
    Err(crate::SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!("must be able to SET ROLE \"{target}\""),
    })
}

pub mod definition;
pub mod dependencies;
pub mod guards;
pub mod inquiry;
pub mod restoration;
