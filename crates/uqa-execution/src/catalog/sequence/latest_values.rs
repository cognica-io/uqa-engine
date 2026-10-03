//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The value state the latest commit holds for each sequence definition generation. Value operations move it outside every transaction and without a catalog change, so a session's catalog cache does not follow it; a reader that shows a sequence's state, as a query of the sequence does, reads it as `PostgreSQL` reads the sequence's current tuple.

mod provider;

pub use provider::ProviderSequenceValues;

use std::collections::HashMap;

use crate::row_locks::SequencePositionKey;
use uqa_storage::{SequenceValuePosition, StorageBackendResult};

/// Reads the committed value state of every sequence definition generation.
pub trait LatestSequenceValues: Send + Sync {
    fn latest_sequence_values(
        &self,
    ) -> StorageBackendResult<HashMap<SequencePositionKey, SequenceValuePosition>>;
}
