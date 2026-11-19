//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persisted sequence definitions retain allocation options and bound or legacy authority.

use super::{RelationIdentity, SequenceOwner, SequenceSecurityRow};
use serde::{Deserialize, Serialize};

/// Durable SQL sequence state. Sequence allocation is implemented by the
/// catalog backend as one atomic mutation so independent engine sessions (and
/// independently opened engines) cannot return the same value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceOptions {
    pub data_type: String,
    /// `None` is accepted only while decoding a legacy row and is resolved from its increment direction by native sequence restoration.
    pub min_value: Option<i64>,
    /// `None` is accepted only while decoding a legacy row and is resolved from its increment direction by native sequence restoration.
    pub max_value: Option<i64>,
    pub cycle: bool,
    #[serde(default = "default_sequence_cache_size")]
    pub cache_size: i64,
}

const fn default_sequence_cache_size() -> i64 {
    1
}

impl Default for SequenceOptions {
    fn default() -> Self {
        Self {
            data_type: "bigint".into(),
            min_value: None,
            max_value: None,
            cycle: false,
            cache_size: default_sequence_cache_size(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceRow {
    pub relation: RelationIdentity,
    /// Captured owner and ACL role identities, or a legacy row awaiting initial-open migration.
    #[serde(flatten)]
    pub security: SequenceSecurityRow,
    /// Stable identity of this sequence incarnation. Dropping and recreating the same qualified name must allocate a different value.
    #[serde(default)]
    pub object_id: [u8; 16],
    /// Changes when value-generation options or persistence replace the sequence's allocation state. Stable across ownership and name changes, value reservations, and `setval`.
    #[serde(default)]
    pub definition_generation: [u8; 16],
    pub start: i64,
    pub increment: i64,
    pub current: i64,
    /// False until the first allocation returns `current` verbatim.
    pub called: bool,
    /// Number of values whose advancement is already durable in the sequence log.
    #[serde(default)]
    pub log_count: i64,
    /// `PostgreSQL` `pg_class.relpersistence` code. Durable sequence rows accept only permanent (`p`) and unlogged (`u`) values.
    pub persistence: String,
    #[serde(default)]
    pub owner: Option<SequenceOwner>,
    #[serde(default)]
    pub options: SequenceOptions,
}
