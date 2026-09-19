//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable schema ownership and access-control metadata shared by catalog consumers.

use crate::catalog_role::{BoundAclEntry, RoleIdentity};
use serde::{Deserialize, Serialize};

/// A namespace lifetime and one replacement of its catalog tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaTupleIdentity {
    pub oid: i64,
    pub object_id: [u8; 16],
    pub revision: [u8; 16],
}

impl SchemaTupleIdentity {
    pub fn is_valid(self) -> bool {
        u32::try_from(self.oid).is_ok_and(|oid| oid != 0)
            && self.object_id != [0; 16]
            && self.revision != [0; 16]
    }

    /// Bootstrap and migrated namespaces retain their original OIDs with database-scoped identities.
    pub fn initial(oid: u32) -> Self {
        let mut object_id = [0; 16];
        object_id[..4].copy_from_slice(&2615_u32.to_be_bytes());
        object_id[12..].copy_from_slice(&oid.to_be_bytes());
        Self {
            oid: i64::from(oid),
            object_id,
            revision: object_id,
        }
    }
}

/// Schema security with role incarnations, independent of current display names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundSchemaRow {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tuple: Option<SchemaTupleIdentity>,
    pub role_owner: RoleIdentity,
    pub acl: Option<Vec<BoundAclEntry<SchemaPrivileges>>>,
}

impl BoundSchemaRow {
    pub fn bootstrap(name: impl Into<String>) -> Self {
        let name = name.into();
        let owner = RoleIdentity::BOOTSTRAP;
        let acl = (name == "public").then(|| {
            vec![
                BoundAclEntry {
                    role: Some(owner),
                    grantor: owner,
                    privileges: SchemaPrivileges::ALL,
                    grant_options: SchemaPrivileges::default(),
                },
                BoundAclEntry {
                    role: None,
                    grantor: owner,
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
            tuple: None,
            role_owner: owner,
            acl,
        }
    }
}

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
    pub role: crate::catalog_acl::AclGrantee,
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
                    role: crate::catalog_acl::AclGrantee::Public,
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
