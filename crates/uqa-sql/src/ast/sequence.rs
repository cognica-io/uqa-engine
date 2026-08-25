//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable provenance for implicit SERIAL and identity sequences.

use serde::{Deserialize, Deserializer, Serialize};

/// `PostgreSQL` syntax that supplies an omitted column value from an implicit sequence. `SERIAL` is a sequence-backed default, while identity columns retain a separate generation attribute and do not have a column default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoIncrementKind {
    Serial,
    IdentityAlways,
    IdentityByDefault,
    /// Catalogs written before sequence provenance was persisted cannot distinguish `SERIAL` from identity syntax. Preserve their historical table-counter behavior instead of guessing and changing stored data.
    Legacy,
}

/// Durable owner of an implicit sequence. Inherited `SERIAL` defaults and declarative partitions copy this owner unchanged, so truncating a child cannot reset a sequence owned by its parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoIncrementOwner {
    pub table: String,
    pub column: String,
}

/// Durable sequence provenance for a `SERIAL` or identity column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoIncrement {
    pub kind: AutoIncrementKind,
    /// Canonical sequence relation name. Compiler-produced column definitions leave this empty until `CREATE TABLE` resolves the destination schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<AutoIncrementOwner>,
}

impl AutoIncrement {
    #[must_use]
    pub const fn serial() -> Self {
        Self {
            kind: AutoIncrementKind::Serial,
            sequence: None,
            owner: None,
        }
    }

    #[must_use]
    pub const fn identity_always() -> Self {
        Self {
            kind: AutoIncrementKind::IdentityAlways,
            sequence: None,
            owner: None,
        }
    }

    #[must_use]
    pub const fn identity_by_default() -> Self {
        Self {
            kind: AutoIncrementKind::IdentityByDefault,
            sequence: None,
            owner: None,
        }
    }

    #[must_use]
    pub const fn legacy() -> Self {
        Self {
            kind: AutoIncrementKind::Legacy,
            sequence: None,
            owner: None,
        }
    }

    #[must_use]
    pub const fn is_identity(&self) -> bool {
        matches!(
            self.kind,
            AutoIncrementKind::IdentityAlways | AutoIncrementKind::IdentityByDefault
        )
    }

    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        matches!(self.kind, AutoIncrementKind::Legacy)
    }
}

pub(super) fn deserialize_auto_increment<'de, D>(
    deserializer: D,
) -> Result<Option<AutoIncrement>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Representation {
        Legacy(bool),
        Provenance(AutoIncrement),
    }

    Ok(match Option::<Representation>::deserialize(deserializer)? {
        Some(Representation::Legacy(true)) => Some(AutoIncrement::legacy()),
        Some(Representation::Legacy(false)) | None => None,
        Some(Representation::Provenance(provenance)) => Some(provenance),
    })
}
