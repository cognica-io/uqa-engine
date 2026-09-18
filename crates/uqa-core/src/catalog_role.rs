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
