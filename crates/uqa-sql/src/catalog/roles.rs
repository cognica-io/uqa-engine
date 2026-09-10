//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role definitions and membership semantics over explicit catalog values.

use crate::ast::{CreateRoleStmt, RoleAttribute};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub mod memberships;
pub use memberships::{role_can_set, role_inherits};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleDefinition {
    pub oid: i64,
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

    pub fn from_create(statement: &CreateRoleStmt) -> Self {
        Self {
            oid: role_oid(&statement.name),
            name: statement.name.clone(),
            attributes: statement.attributes.clone(),
            connection_limit: statement.connection_limit,
        }
    }

    pub fn has(&self, attribute: RoleAttribute) -> bool {
        self.attributes.contains(&attribute)
    }
}

pub fn role_oid(name: &str) -> i64 {
    if name == "uqa" {
        return 10;
    }
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    20_000 + i64::try_from(hash % 2_000_000_000).unwrap_or(0)
}
