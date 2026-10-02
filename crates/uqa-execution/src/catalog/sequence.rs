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

impl SequenceState {
    /// The key of this allocation generation's position.
    #[must_use]
    pub const fn position_key(&self, object_id: [u8; 16]) -> crate::row_locks::SequencePositionKey {
        crate::row_locks::SequencePositionKey {
            object: object_id,
            definition: self.definition_generation,
        }
    }

    /// The state at the exact position recorded for it. The value state held here is the durable record's, which runs ahead of the values handed out while a position is recorded.
    #[must_use]
    pub fn at_position(
        mut self,
        recorded: Option<crate::row_locks::RecordedSequencePosition>,
    ) -> Self {
        if let Some(position) =
            recorded.and_then(|recorded| recorded.continuing((self.current, self.called)))
        {
            self.current = position.current;
            self.called = position.called;
            self.log_count = position.log_count;
        }
        self
    }
}

use super::security::BoundSequenceSecurity;
use uqa_core::RelationIdentity;
use uqa_storage::{SequenceOptions, SequenceRow, StorageBackendError, StorageBackendResult};

pub fn sequence_row(
    name: &str,
    object_id: [u8; 16],
    state: SequenceState,
    persistence: uqa_sql::ast::RelationPersistence,
    security: &BoundSequenceSecurity,
) -> StorageBackendResult<SequenceRow> {
    Ok(SequenceRow {
        relation: RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?,
        security: security.row().into(),
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
pub mod snapshot;
pub mod values;
