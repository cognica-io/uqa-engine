//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable role identities shared by SQL catalogs and storage metadata.

use serde::{Deserialize, Serialize};

/// A catalog reference retains both the public OID and the role incarnation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoleIdentity {
    pub oid: i64,
    pub object_id: [u8; 16],
}

impl RoleIdentity {
    pub const BOOTSTRAP: Self = Self {
        oid: 10,
        object_id: *b"UQA:role00000010",
    };

    pub fn is_valid(self) -> bool {
        self.oid > 0 && u32::try_from(self.oid).is_ok() && self.object_id != [0; 16]
    }
}

/// One ACL path whose endpoints retain their original role incarnations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundAclEntry<Privileges> {
    /// An explicit null grantee represents PUBLIC; a missing field is invalid.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub role: Option<RoleIdentity>,
    pub grantor: RoleIdentity,
    pub privileges: Privileges,
    pub grant_options: Privileges,
}

#[cfg(test)]
mod tests;
