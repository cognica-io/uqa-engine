//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
pub mod columns;
pub mod table;
pub use uqa_core::catalog_acl::{TableAclEntry, TablePrivileges};

/// Complete table-shaped relation security state. Ownership and ACL changes are published through one value so readers cannot observe a torn authorization state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSecurity {
    pub role_owner: String,
    pub acl: Option<Vec<TableAclEntry>>,
    pub column_acls: BTreeMap<String, Vec<TableAclEntry>>,
}

impl TableSecurity {
    pub fn owner(role_owner: impl Into<String>) -> Self {
        Self {
            role_owner: role_owner.into(),
            acl: None,
            column_acls: BTreeMap::new(),
        }
    }
}
