//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Own sequence discard state, transaction history and physical session selection.
use crate::{Engine, NontransactionalSequenceValue, StorageBackendResult};
impl Engine {
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
            for history in frame.nontransactional_sequence_values.values_mut() {
                history.session_currval = None;
                history.defines_lastval = false;
            }
        }
    }
    pub(super) fn record_nontransactional_sequence_value(
        &self,
        definition_generation: [u8; 16],
        value: NontransactionalSequenceValue,
        defines_lastval: bool,
    ) {
        let session_currval = self
            .session
            .state
            .read()
            .sequence_currvals
            .values()
            .find(|current| current.object_id == value.object_id)
            .copied();
        let mut transactions = self.session.transactions.lock();
        for frame in transactions.iter_mut() {
            if defines_lastval {
                for history in frame.nontransactional_sequence_values.values_mut() {
                    history.defines_lastval = false;
                }
            }
            let history = frame
                .nontransactional_sequence_values
                .entry(value.object_id)
                .or_default();
            let preserves_lastval =
                !defines_lastval && history.object_id == value.object_id && history.defines_lastval;
            history
                .values_by_definition
                .insert(definition_generation, value);
            history.object_id = value.object_id;
            history.session_currval = session_currval;
            history.defines_lastval = defines_lastval || preserves_lastval;
        }
    }
    pub(super) fn open_nontransactional_sequence_session(
        &self,
    ) -> StorageBackendResult<Option<uqa_storage::PersistentStorageSession>> {
        if !self.backend_transaction_is_deferred()
            || self.session.row_lock_statements.lock().is_empty()
        {
            return Ok(None);
        }
        self.storage
            .provider
            .as_ref()
            .map_or(Ok(None), |provider| provider.open_session().map(Some))
    }
}
