//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read committed sequence value records through an independent session of the storage provider.

use std::{collections::HashMap, sync::Arc};

use super::LatestSequenceValues;
use crate::{catalog::snapshot_read::with_read_transaction, row_locks::SequencePositionKey};
use uqa_storage::{PersistentStorageProvider, SequenceValuePosition, StorageBackendResult};

/// Reads the value records of the latest commit in a read transaction of its own, which leaves the caller's transaction and snapshot alone.
pub struct ProviderSequenceValues {
    provider: Arc<dyn PersistentStorageProvider>,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}

impl ProviderSequenceValues {
    pub fn new(provider: Arc<dyn PersistentStorageProvider>) -> Self {
        Self {
            provider,
            _memory: None,
        }
    }

    pub fn shared_with_control(
        provider: Arc<dyn PersistentStorageProvider>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Arc<Self>, uqa_sql::SQLError> {
        let memory = control.reserve(size_of::<Self>())?;
        Ok(Arc::new(Self {
            provider,
            _memory: memory,
        }))
    }
}

impl LatestSequenceValues for ProviderSequenceValues {
    fn latest_sequence_values(
        &self,
    ) -> StorageBackendResult<HashMap<SequencePositionKey, SequenceValuePosition>> {
        let session = self.provider.open_session()?;
        let rows = with_read_transaction(&session, |catalog| catalog.load_sequence_rows())?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    SequencePositionKey {
                        object: row.object_id,
                        definition: row.definition_generation,
                    },
                    SequenceValuePosition {
                        current: row.current,
                        called: row.called,
                        log_count: row.log_count,
                    },
                )
            })
            .collect())
    }
}
