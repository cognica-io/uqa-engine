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
pub mod memberships;
pub use memberships::{role_can_set, role_inherits};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleDefinition {
    pub oid: i64,
    /// Durable incarnation independent of the recyclable SQL-visible OID. Zero identifies legacy metadata that requires initial-open migration.
    #[serde(default)]
    pub object_id: [u8; 16],
    pub name: String,
    pub attributes: BTreeSet<RoleAttribute>,
    pub connection_limit: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoleMembershipKey {
    pub role: String,
    pub member: String,
    pub grantor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleMembership {
    pub oid: i64,
    pub role: String,
    pub member: String,
    pub grantor: String,
    pub admin_option: bool,
    pub inherit_option: bool,
    pub set_option: bool,
}

impl RoleMembership {
    pub fn key(&self) -> RoleMembershipKey {
        RoleMembershipKey {
            role: self.role.clone(),
            member: self.member.clone(),
            grantor: self.grantor.clone(),
        }
    }
}

impl RoleDefinition {
    pub fn bootstrap() -> Self {
        Self {
            oid: 10,
            object_id: *b"UQA:role00000010",
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
            name: statement.name.clone(),
            attributes: statement.attributes.clone(),
            connection_limit: statement.connection_limit,
        }
    }

    pub fn has(&self, attribute: RoleAttribute) -> bool {
        self.attributes.contains(&attribute)
    }
}

/// Session names used by `CURRENT_USER` and `SESSION_USER` role references.
pub trait RoleReferenceNames {
    fn current_user_name(&self) -> String;
    fn session_user_name(&self) -> String;
}
pub fn resolve_role_reference(names: &dyn RoleReferenceNames, name: &str) -> String {
    match name {
        "CURRENT_USER" => names.current_user_name(),
        "SESSION_USER" => names.session_user_name(),
        other => other.to_string(),
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
    current: &str,
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
