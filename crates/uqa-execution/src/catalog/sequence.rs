//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_sql::ast::SequenceDataType;
use uqa_storage::SequenceOwner;

mod decode;

/// Mutable state of a single SQL sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct SequenceState {
    pub start: i64,
    pub increment: i64,
    pub current: i64,
    /// Whether `current` has already been returned by `nextval`.  Keeping this
    /// bit avoids the lossy `start - increment` sentinel at BIGINT boundaries.
    #[serde(default = "sequence_state_called_default")]
    pub called: bool,
    #[serde(default)]
    pub log_count: i64,
    pub data_type: SequenceDataType,
    pub min_value: i64,
    pub max_value: i64,
    pub cycle: bool,
    #[serde(default = "sequence_cache_size_default")]
    pub cache_size: i64,
    #[serde(default)]
    pub definition_generation: [u8; 16],
    #[serde(default)]
    pub owner: Option<SequenceOwner>,
}

const fn sequence_state_called_default() -> bool {
    // Legacy serialized states used `current = start - increment`; treating
    // that value as called preserves their next allocation semantics.
    true
}

const fn sequence_cache_size_default() -> i64 {
    1
}
