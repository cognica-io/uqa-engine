//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One ACL entry of an object with a single grantable privilege, such as a routine's `EXECUTE` or a type's `USAGE`.

use serde::{Deserialize, Serialize};

/// One explicit ACL entry. An absent ACL keeps the object's `PostgreSQL` default: the privilege for PUBLIC and the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectAclEntry {
    /// An explicit null means PUBLIC; a missing grantee is invalid.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub role: Option<uqa_core::catalog_role::RoleIdentity>,
    pub grantor: uqa_core::catalog_role::RoleIdentity,
    pub grant_option: bool,
}
