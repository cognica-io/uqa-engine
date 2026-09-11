//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_sql::ast::SequenceDataType;
use uqa_storage::SequenceOwner;

mod decode;
mod definition;

pub use definition::altered_sequence_state;

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

use super::security::SequenceSecurity;
use uqa_core::RelationIdentity;
use uqa_storage::{SequenceOptions, SequenceRow, StorageBackendError, StorageBackendResult};

pub fn sequence_row(
    name: &str,
    object_id: [u8; 16],
    state: SequenceState,
    persistence: uqa_sql::ast::RelationPersistence,
    security: &SequenceSecurity,
) -> StorageBackendResult<SequenceRow> {
    Ok(SequenceRow {
        relation: RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?,
        role_owner: security.role_owner.clone(),
        acl: security.acl.clone(),
        object_id,
        definition_generation: state.definition_generation,
        start: state.start,
        increment: state.increment,
        current: state.current,
        called: state.called,
        log_count: state.log_count,
        persistence: persistence.catalog_code().into(),
        owner: state.owner,
        options: SequenceOptions {
            data_type: state.data_type.sql_name().into(),
            min_value: Some(state.min_value),
            max_value: Some(state.max_value),
            cycle: state.cycle,
            cache_size: state.cache_size,
        },
    })
}

pub mod restoration;

pub mod session;
pub mod values;
