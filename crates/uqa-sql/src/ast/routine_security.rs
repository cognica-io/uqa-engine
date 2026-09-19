//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine security, ownership, configuration, privileges, and role statements.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    AclRoleSpecification, FunctionVolatility, RoleSpecification, RoutineColumnTypeReference,
};

/// `PARALLEL UNSAFE`, `PARALLEL RESTRICTED`, or `PARALLEL SAFE` routine metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FunctionParallel {
    #[default]
    Unsafe,
    Restricted,
    Safe,
}

/// Execution identity and planner leakproofness attached to a routine definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RoutineSecurityAttributes {
    /// `SECURITY DEFINER` when true, otherwise `SECURITY INVOKER`.
    #[serde(default)]
    pub security_definer: bool,
    /// `LEAKPROOF` planner metadata.
    #[serde(default)]
    pub leakproof: bool,
}

/// One explicit `EXECUTE` ACL entry. `None` on `CreateFunction::execute_acl` retains `PostgreSQL`'s default public execution privilege.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineAclEntry {
    /// An explicit null means PUBLIC; a missing grantee is invalid.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub role: Option<uqa_core::catalog_role::RoleIdentity>,
    pub grantor: uqa_core::catalog_role::RoleIdentity,
    pub grant_option: bool,
}

/// Old parsed declarations used an empty owner string. This accepts only that unbound marker; stored authority is separately required to carry a valid identity.
pub(super) fn deserialize_routine_owner<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<uqa_core::catalog_role::RoleIdentity>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Owner {
        Bound(Option<uqa_core::catalog_role::RoleIdentity>),
        Unbound(String),
    }
    match Owner::deserialize(deserializer)? {
        Owner::Bound(identity) => Ok(identity),
        Owner::Unbound(name) if name.is_empty() => Ok(None),
        Owner::Unbound(_) => Err(serde::de::Error::custom(
            "routine owner requires a role identity",
        )),
    }
}

/// Routine namespace selected by `ALTER FUNCTION`, `ALTER PROCEDURE`, or `ALTER ROUTINE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlterRoutineKind {
    Function,
    Procedure,
    Routine,
}

/// `ALTER FUNCTION | PROCEDURE | ROUTINE name[(input_types)] ...` with an optional exact declared input identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlterRoutineStmt {
    pub kind: AlterRoutineKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arg_type_references: Vec<Option<RoutineColumnTypeReference>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volatility: Option<FunctionVolatility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_definer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leakproof: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel: Option<FunctionParallel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_actions: Vec<RoutineConfigAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutineConfigAction {
    Set { name: String, value: String },
    FromCurrent { name: String },
    Reset { name: String },
    ResetAll,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlterRoutineOwnerStmt {
    pub kind: AlterRoutineKind,
    pub name: String,
    pub arg_types: Option<Vec<String>>,
    pub arg_type_references: Vec<Option<RoutineColumnTypeReference>>,
    pub new_owner: RoleSpecification,
}

/// `ALTER FUNCTION | PROCEDURE | ROUTINE name[(input_types)] RENAME TO new_name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameRoutineStmt {
    pub kind: AlterRoutineKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arg_type_references: Vec<Option<RoutineColumnTypeReference>>,
    pub new_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRoutineItem {
    pub name: String,
    pub arg_types: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutineRevokeBehavior {
    #[default]
    Restrict,
    Cascade,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRoutineStmt {
    pub kind: AlterRoutineKind,
    pub is_grant: bool,
    pub grant_option: bool,
    pub grant_option_only: bool,
    pub items: Vec<GrantRoutineItem>,
    pub grantees: Vec<AclRoleSpecification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantor: Option<RoleSpecification>,
    #[serde(default)]
    pub revoke_behavior: RoutineRevokeBehavior,
}

/// `PostgreSQL` 18 options stored on one role-membership grant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleMembershipOptions {
    pub admin: Option<bool>,
    pub inherit: Option<bool>,
    pub set: Option<bool>,
}

/// `GRANT role TO member` or `REVOKE role FROM member`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRoleStmt {
    pub granted_roles: Vec<String>,
    pub grantee_roles: Vec<RoleSpecification>,
    pub is_grant: bool,
    pub options: RoleMembershipOptions,
    pub grantor: Option<RoleSpecification>,
    pub cascade: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RoleAttribute {
    Superuser,
    Inherit,
    CreateRole,
    CreateDb,
    Login,
    Replication,
    BypassRls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateRoleStmt {
    pub name: String,
    pub attributes: BTreeSet<RoleAttribute>,
    pub connection_limit: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub in_roles: Vec<RoleSpecification>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub role_members: Vec<RoleSpecification>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admin_members: Vec<RoleSpecification>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoleMembershipAction {
    Add,
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlterRoleStmt {
    pub name: RoleSpecification,
    pub attributes: BTreeMap<RoleAttribute, bool>,
    pub connection_limit: Option<i32>,
    pub membership_action: Option<RoleMembershipAction>,
    pub members: Vec<RoleSpecification>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameRoleStmt {
    pub name: String,
    pub new_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DropRoleStmt {
    pub names: Vec<RoleSpecification>,
    pub if_exists: bool,
}
