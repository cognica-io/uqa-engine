//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable access-control metadata for table-shaped relations.

use serde::{Deserialize, Serialize};

/// Grantable privileges carried by one table-shaped relation ACL path.
#[expect(
    clippy::struct_excessive_bools,
    reason = "models PostgreSQL's independently grantable table-shaped relation privileges"
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TablePrivileges {
    #[serde(default)]
    pub select: bool,
    #[serde(default)]
    pub insert: bool,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub delete: bool,
    #[serde(default)]
    pub truncate: bool,
    #[serde(default)]
    pub references: bool,
    #[serde(default)]
    pub trigger: bool,
    #[serde(default)]
    pub maintain: bool,
}

impl TablePrivileges {
    pub const ALL: Self = Self {
        select: true,
        insert: true,
        update: true,
        delete: true,
        truncate: true,
        references: true,
        trigger: true,
        maintain: true,
    };

    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.select
            && !self.insert
            && !self.update
            && !self.delete
            && !self.truncate
            && !self.references
            && !self.trigger
            && !self.maintain
    }

    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.select && other.select
            || self.insert && other.insert
            || self.update && other.update
            || self.delete && other.delete
            || self.truncate && other.truncate
            || self.references && other.references
            || self.trigger && other.trigger
            || self.maintain && other.maintain
    }

    pub fn insert(&mut self, other: Self) {
        self.select |= other.select;
        self.insert |= other.insert;
        self.update |= other.update;
        self.delete |= other.delete;
        self.truncate |= other.truncate;
        self.references |= other.references;
        self.trigger |= other.trigger;
        self.maintain |= other.maintain;
    }

    pub fn remove(&mut self, other: Self) {
        self.select &= !other.select;
        self.insert &= !other.insert;
        self.update &= !other.update;
        self.delete &= !other.delete;
        self.truncate &= !other.truncate;
        self.references &= !other.references;
        self.trigger &= !other.trigger;
        self.maintain &= !other.maintain;
    }
}

/// One explicit table-shaped relation ACL path. Legacy entries without an explicit grantor originate from the relation owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableAclEntry {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantor: Option<String>,
    #[serde(default)]
    pub privileges: TablePrivileges,
    #[serde(default)]
    pub grant_options: TablePrivileges,
}
