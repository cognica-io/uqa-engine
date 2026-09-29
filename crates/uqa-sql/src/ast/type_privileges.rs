//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `GRANT | REVOKE ... ON TYPE | DOMAIN`.

use serde::{Deserialize, Serialize};

use super::{AclRoleSpecification, RoleSpecification, TypeObjectKind};

/// A privilege named by `GRANT | REVOKE ... ON TYPE | DOMAIN`. Types have only `USAGE`; other names are rejected after the targets are resolved, as `PostgreSQL` does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypePrivilege {
    Usage,
    Unsupported(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeRevokeBehavior {
    Restrict,
    Cascade,
}

/// `GRANT | REVOKE [GRANT OPTION FOR] privileges ON TYPE | DOMAIN names TO | FROM grantees [WITH GRANT OPTION] [GRANTED BY role] [CASCADE | RESTRICT]`. An empty privilege list is `ALL [PRIVILEGES]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantTypeStmt {
    pub kind: TypeObjectKind,
    pub is_grant: bool,
    pub grant_option: bool,
    pub grant_option_only: bool,
    pub privileges: Vec<TypePrivilege>,
    /// Rendered, possibly qualified type names as written.
    pub names: Vec<String>,
    pub grantees: Vec<AclRoleSpecification>,
    pub grantor: Option<RoleSpecification>,
    pub revoke_behavior: TypeRevokeBehavior,
}
