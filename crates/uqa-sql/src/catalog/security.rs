//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
pub mod columns;
pub mod schema;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaSecurity {
    pub role_owner: String,
    pub acl: Option<Vec<uqa_core::catalog_schema::SchemaAclEntry>>,
}

impl SchemaSecurity {
    pub fn from_row(row: uqa_core::catalog_schema::SchemaRow) -> (String, Self) {
        (
            row.name,
            Self {
                role_owner: row.role_owner,
                acl: row.acl,
            },
        )
    }

    pub fn row(&self, name: impl Into<String>) -> uqa_core::catalog_schema::SchemaRow {
        uqa_core::catalog_schema::SchemaRow {
            name: name.into(),
            role_owner: self.role_owner.clone(),
            acl: self.acl.clone(),
        }
    }

    pub fn legacy(name: &str) -> Self {
        let (_, security) = Self::from_row(uqa_core::catalog_schema::SchemaRow::legacy(name));
        security
    }
}
