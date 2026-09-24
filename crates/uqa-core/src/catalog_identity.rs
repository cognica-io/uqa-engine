//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A catalog object's durable incarnation and public address, independent of its display name.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogObjectIdentity {
    pub object_id: [u8; 16],
    pub oid: i64,
}

impl CatalogObjectIdentity {
    pub fn is_valid(self) -> bool {
        self.object_id != [0; 16] && u32::try_from(self.oid).is_ok_and(|oid| oid != 0)
    }
}
