//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authorization values and ACL operations for catalog execution.

pub mod columns;
pub mod schema;
pub mod sequence;
pub mod table;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatabasePrivileges {
    pub connect: bool,
    pub create: bool,
    pub temporary: bool,
}

impl DatabasePrivileges {
    pub const ALL: Self = Self {
        connect: true,
        create: true,
        temporary: true,
    };

    pub const fn intersects(self, other: Self) -> bool {
        (self.connect && other.connect)
            || (self.create && other.create)
            || (self.temporary && other.temporary)
    }

    pub fn insert(&mut self, other: Self) {
        self.connect |= other.connect;
        self.create |= other.create;
        self.temporary |= other.temporary;
    }

    pub fn remove(&mut self, other: Self) {
        self.connect &= !other.connect;
        self.create &= !other.create;
        self.temporary &= !other.temporary;
    }

    pub const fn is_empty(self) -> bool {
        !self.connect && !self.create && !self.temporary
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatabaseAclEntry {
    pub role: String,
    pub grantor: Option<String>,
    pub privileges: DatabasePrivileges,
    pub grant_options: DatabasePrivileges,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatabaseSecurity {
    pub role_owner: String,
    pub acl: Option<Vec<DatabaseAclEntry>>,
}

impl DatabaseSecurity {
    pub fn bootstrap() -> Self {
        Self {
            role_owner: "uqa".into(),
            acl: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceSecurity {
    pub role_owner: String,
    pub acl: Option<Vec<uqa_storage::SequenceAclEntry>>,
}

pub use uqa_sql::catalog::security::TableSecurity;

pub use uqa_sql::catalog::security::SchemaSecurity;

pub mod roles;
