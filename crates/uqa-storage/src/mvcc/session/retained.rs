//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained readers compose only boundaries owned by the same active transaction.

use std::sync::Arc;

use super::{SerializableReadContext, VersionedKeyValueStore};
use crate::mvcc::{CommittedRecordSnapshot, DatabaseId, MergedRecordSnapshot, VersionError};
use crate::read_control::StorageReadControl;
use crate::{StorageBackendError, StorageBackendResult, StorageSessionAffinity};

/// An immutable committed boundary captured by one active transaction. It cannot be transferred to an independent session or reused after transaction completion, even when no commit sequence changed. Cloning the Arc retains the original snapshot and allowance without copying records.
pub struct CommittedReadBoundary {
    database: DatabaseId,
    session: StorageSessionAffinity,
    transaction: Arc<()>,
    committed: Arc<dyn CommittedRecordSnapshot>,
    serializable: Option<SerializableReadContext>,
    _memory: uqa_core::memory::MemoryReservation,
}

impl VersionedKeyValueStore {
    /// Retain only the committed input of the current transaction. Later private writes and savepoint branches can be attached explicitly through `new_read_session_with_private_changes`.
    pub fn capture_committed_read_boundary(
        &self,
    ) -> StorageBackendResult<Arc<CommittedReadBoundary>> {
        self.require_mutable_session()?;
        self.control.check()?;
        let active = self.active.lock();
        let transaction = active.as_ref().ok_or_else(super::no_transaction)?;
        transaction
            .unsealed()
            .map_err(VersionError::into_storage_error)?;
        let memory = self
            .control
            .memory()
            .reserve(std::mem::size_of::<CommittedReadBoundary>())?;
        Ok(Arc::new(CommittedReadBoundary {
            database: self.persistence.database_id(),
            session: self.affinity.clone(),
            transaction: Arc::clone(&transaction.retention_identity),
            committed: transaction.committed_snapshot(),
            serializable: transaction.serializable_context().cloned(),
            _memory: memory,
        }))
    }

    /// Retain the selected committed boundary plus this command's current private root. The returned reader is immutable and cannot complete its source transaction. A foreign session, completed transaction, changed SSI participant or newer-than-current boundary is rejected before publication.
    pub fn new_read_session_with_private_changes(
        &self,
        boundary: &CommittedReadBoundary,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<Self> {
        self.require_mutable_session()?;
        cancellation.check()?;
        if boundary.database != self.persistence.database_id() || boundary.session != self.affinity
        {
            return Err(invalid_boundary(
                "committed read boundary belongs to another storage session",
            ));
        }
        let active = self.active.lock();
        let transaction = active.as_ref().ok_or_else(super::no_transaction)?;
        transaction
            .unsealed()
            .map_err(VersionError::into_storage_error)?;
        if !Arc::ptr_eq(&boundary.transaction, &transaction.retention_identity) {
            return Err(invalid_boundary(
                "committed read boundary belongs to a completed transaction",
            ));
        }
        if boundary
            .serializable
            .as_ref()
            .map(SerializableReadContext::id)
            != transaction
                .serializable_context()
                .map(SerializableReadContext::id)
        {
            return Err(invalid_boundary(
                "committed read boundary has another serializable participant",
            ));
        }
        let current = transaction
            .view()
            .map_err(VersionError::into_storage_error)?;
        if boundary.committed.sequence() > current.sequence() {
            return Err(invalid_boundary(
                "committed read boundary is newer than the current command",
            ));
        }
        let retained = current.with_committed_snapshot(
            Arc::clone(&boundary.committed),
            boundary.serializable.clone(),
        );
        cancellation.check()?;
        Ok(self.read_session_at(retained, cancellation))
    }

    /// Open an independently owned read-only session at this exact committed/private view. Its original participant, snapshot leases and retention allowance remain shared, while read cancellation stays independent. Completing this reader never completes the originating transaction.
    pub fn new_retained_read_session(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<Self> {
        cancellation.check()?;
        let active = self.active.lock();
        let retained = self
            .view_locked(active.as_ref())
            .map_err(VersionError::into_storage_error)?;
        Ok(self.read_session_at(retained, cancellation))
    }

    fn read_session_at(
        &self,
        retained: MergedRecordSnapshot,
        cancellation: &uqa_core::CancellationToken,
    ) -> Self {
        let mut session = self.new_session_with_cancellation(cancellation);
        session.control =
            StorageReadControl::new(self.control.memory(), session.control.cancellation());
        session.retained = Some(retained);
        session.sequence_durability = Arc::clone(&self.sequence_durability);
        session.sequence_generation = Some(
            self.sequence_generation
                .unwrap_or_else(|| self.sequence_durability.generation()),
        );
        session
    }
}

fn invalid_boundary(message: &str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}
