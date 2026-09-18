//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable table-shaped ownership and ACL data, independent of SQL binding.

use super::{TableAclEntry, TablePrivileges};
use crate::catalog_role::{BoundAclEntry, RoleIdentity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundRelationSecurity {
    pub role_owner: RoleIdentity,
    /// Explicit null preserves the owner-only default ACL; an absent current field is invalid.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub acl: Option<Vec<BoundAclEntry<TablePrivileges>>>,
    pub column_acls: BTreeMap<String, Vec<BoundAclEntry<TablePrivileges>>>,
}

impl BoundRelationSecurity {
    pub fn owner(role_owner: RoleIdentity) -> Self {
        Self {
            role_owner,
            acl: None,
            column_acls: BTreeMap::new(),
        }
    }
}

/// Names are accepted only as an input to initial catalog restoration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyRelationSecurity {
    pub role_owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl: Option<Vec<TableAclEntry>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub column_acls: BTreeMap<String, Vec<TableAclEntry>>,
}

impl LegacyRelationSecurity {
    pub fn owner(role_owner: impl Into<String>) -> Self {
        Self {
            role_owner: role_owner.into(),
            acl: None,
            column_acls: BTreeMap::new(),
        }
    }
}
