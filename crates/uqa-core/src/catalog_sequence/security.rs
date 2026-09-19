//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable sequence authority retains role identities independently of display names.

use super::{SequenceAclEntry, SequencePrivileges};
use crate::catalog_role::{BoundAclEntry, RoleIdentity};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundSequenceSecurity {
    pub role_owner: RoleIdentity,
    /// Explicit null preserves default owner privileges; a missing current field is invalid.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub acl: Option<Vec<BoundAclEntry<SequencePrivileges>>>,
}

impl BoundSequenceSecurity {
    pub fn owner(role_owner: RoleIdentity) -> Self {
        Self {
            role_owner,
            acl: None,
        }
    }
}

/// Named authority is accepted only as input to complete initial catalog restoration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacySequenceSecurity {
    #[serde(default = "legacy_owner")]
    pub role_owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl: Option<Vec<SequenceAclEntry>>,
}

impl LegacySequenceSecurity {
    pub fn owner(role_owner: impl Into<String>) -> Self {
        Self {
            role_owner: role_owner.into(),
            acl: None,
        }
    }
}

fn legacy_owner() -> String {
    "uqa".into()
}
