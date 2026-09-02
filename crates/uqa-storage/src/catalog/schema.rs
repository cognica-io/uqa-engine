//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

/// Grantable privileges carried by one schema ACL path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaPrivileges {
    #[serde(default)]
    pub usage: bool,
    #[serde(default)]
    pub create: bool,
}

impl SchemaPrivileges {
    pub const ALL: Self = Self {
        usage: true,
        create: true,
    };

    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.usage && !self.create
    }

    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.usage && other.usage || self.create && other.create
    }

    pub fn insert(&mut self, other: Self) {
        self.usage |= other.usage;
        self.create |= other.create;
    }

    pub fn remove(&mut self, other: Self) {
        self.usage &= !other.usage;
        self.create &= !other.create;
    }
}

/// One explicit schema ACL path. `None` on [`SchemaRow::acl`] retains the owner-only default privileges of an ordinary newly created schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaAclEntry {
    pub role: String,
    /// Legacy persisted entries without an explicit grantor originate from the schema owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantor: Option<String>,
    #[serde(default)]
    pub privileges: SchemaPrivileges,
    #[serde(default)]
    pub grant_options: SchemaPrivileges,
}

/// Durable schema ownership and ACL metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaRow {
    pub name: String,
    /// SQL role that owns the schema. Catalogs written before schema security belonged to the bootstrap role.
    #[serde(default = "default_schema_role_owner")]
    pub role_owner: String,
    /// Explicit ACL paths. `None` represents the owner-only default for an ordinary schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl: Option<Vec<SchemaAclEntry>>,
}

impl SchemaRow {
    #[must_use]
    pub fn legacy(name: impl Into<String>) -> Self {
        let name = name.into();
        let acl = (name == "public").then(|| {
            vec![
                SchemaAclEntry {
                    role: "uqa".into(),
                    grantor: Some("uqa".into()),
                    privileges: SchemaPrivileges::ALL,
                    grant_options: SchemaPrivileges::default(),
                },
                SchemaAclEntry {
                    role: "PUBLIC".into(),
                    grantor: Some("uqa".into()),
                    privileges: SchemaPrivileges {
                        usage: true,
                        create: false,
                    },
                    grant_options: SchemaPrivileges::default(),
                },
            ]
        });
        Self {
            name,
            role_owner: default_schema_role_owner(),
            acl,
        }
    }
}

fn default_schema_role_owner() -> String {
    "uqa".into()
}
