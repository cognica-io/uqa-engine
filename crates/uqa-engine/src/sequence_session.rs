//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Own sequence discard state, transaction history and physical session selection.
use crate::{Engine, EngineDataSnapshot, NontransactionalSequenceSource, StorageBackendResult};
impl Engine {
    pub(super) fn transaction_sequence_values(
        snapshot: Option<&EngineDataSnapshot>,
    ) -> NontransactionalSequenceSource {
        snapshot
            .and_then(|snapshot| snapshot.data.sources.as_ref())
            .map_or_else(
                || {
                    std::sync::Arc::new(uqa_storage::memory_capture::MemoryCaptureCell::new(
                        uqa_execution::catalog::sequence::capture::SequenceCaptureSource::default(),
                    ))
                },
                crate::state::SnapshotSources::sequence_history,
            )
    }

    pub(super) fn discard_sequence_session_values(&self) {
        self.session.sequence_caches.lock().clear();
        {
            let mut session = self.session.state.write();
            session.sequence_discard_generation =
                session.sequence_discard_generation.wrapping_add(1);
            session.sequence_currvals.clear();
            session.last_sequence = None;
        }
        for frame in self.session.transactions.lock().iter_mut() {
            frame
                .nontransactional_sequence_values
                .write()
                .discard_session_values();
        }
    }
    pub(super) fn open_independent_catalog_session(
        &self,
        cancellation: Option<&uqa_core::CancellationToken>,
    ) -> StorageBackendResult<Option<uqa_storage::PersistentStorageSession>> {
        if !self.versioned_backend_transactions()
            && (!self.backend_transaction_is_deferred()
                || self.session.row_lock_statements.lock().is_empty())
        {
            return Ok(None);
        }
        self.storage.provider.as_ref().map_or(Ok(None), |provider| {
            match cancellation {
                Some(cancellation) => provider.open_session_with_cancellation(cancellation),
                None => provider.open_session(),
            }
            .map(Some)
        })
    }
}
