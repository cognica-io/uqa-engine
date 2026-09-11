//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared persisted sequence ACL and stable owner identity values.

use serde::{Deserialize, Serialize};

/// Grantable privileges carried by one sequence ACL path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequencePrivileges {
    #[serde(default)]
    pub select: bool,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub usage: bool,
}

impl SequencePrivileges {
    pub const ALL: Self = Self {
        select: true,
        update: true,
        usage: true,
    };

    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.select && !self.update && !self.usage
    }

    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.select && other.select || self.update && other.update || self.usage && other.usage
    }

    pub fn insert(&mut self, other: Self) {
        self.select |= other.select;
        self.update |= other.update;
        self.usage |= other.usage;
    }

    pub fn remove(&mut self, other: Self) {
        self.select &= !other.select;
        self.update &= !other.update;
        self.usage &= !other.usage;
    }
}

/// One explicit sequence ACL path. `None` on `SequenceRow::acl` retains `PostgreSQL`'s default owner-only privileges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceAclEntry {
    pub role: String,
    /// Legacy persisted entries without an explicit grantor originate from the sequence owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantor: Option<String>,
    #[serde(default)]
    pub privileges: SequencePrivileges,
    #[serde(default)]
    pub grant_options: SequencePrivileges,
}

/// Dependency strength of a sequence owner. Ordinary `OWNED BY` and `SERIAL` use an automatic dependency, while an identity column owns its sequence through an internal dependency that cannot be reassigned or dropped directly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceOwnerDependency {
    #[default]
    Automatic,
    Internal,
}

impl SequenceOwnerDependency {
    #[must_use]
    pub const fn catalog_code(self) -> &'static str {
        match self {
            Self::Automatic => "a",
            Self::Internal => "i",
        }
    }
}

/// Stable owner identity for a sequence dependency. Names are deliberately excluded so table and column renames do not require dependency rewrites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceOwner {
    pub table_object_id: [u8; 16],
    pub column_object_id: [u8; 16],
    #[serde(default)]
    pub dependency: SequenceOwnerDependency,
}

#[cfg(test)]
mod tests;
