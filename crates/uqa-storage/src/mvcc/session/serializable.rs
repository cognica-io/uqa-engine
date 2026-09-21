//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original participant attribution shared by session and retained record views.

use std::sync::Arc;

use crate::mvcc::{
    SafeSnapshot, SerializableGraph, SerializableParticipant, SerializablePredicate,
    SerializableTransactionId, VersionError, VersionResult, VersionedPersistence,
};
use crate::read_control::StorageReadControl;
use crate::StorageBackendResult;

/// Logical transaction characteristics, independent of whether the physical session permits private writes. Deferrable waiting applies only to a read-only participant.
#[derive(Clone, Copy, Debug, Default)]
pub struct SerializableSnapshotOptions {
    pub read_only: bool,
    pub deferrable: bool,
}

/// Scope one candidate snapshot capture together with the caller's publication baseline. Invoke the supplied operation exactly once, without executing application work. Common storage may request another candidate before returning a safe deferrable snapshot; each call must release its guards before returning so overlapping writers can finish during the wait. An already admitted session retains its original participant without invoking this scope.
pub type SerializableSnapshotCapture<'a> =
    dyn FnMut(&mut dyn FnMut() -> VersionResult<()>) -> VersionResult<()> + 'a;

/// Logical SSI capabilities of one original or retained storage session. Providers forward this boundary without deriving predicates from physical record keys. SQL must cover its access paths before enabling automatic admission.
pub trait SerializableSession: Send + Sync {
    fn establish_serializable_snapshot(&self) -> StorageBackendResult<SerializableReadContext>;
    /// Select the first snapshot using logical characteristics and a caller-scoped capture. Existing participants retain their original classification without another capture. Only read-only deferrable admission waits for a proven safe snapshot.
    fn establish_serializable_snapshot_with(
        &self,
        options: SerializableSnapshotOptions,
        capture: &mut SerializableSnapshotCapture<'_>,
    ) -> StorageBackendResult<SerializableReadContext>;
    fn serializable_read_context(&self) -> StorageBackendResult<Option<SerializableReadContext>>;
    fn observe_serializable_write(
        &self,
        predicate: SerializablePredicate<'_>,
    ) -> StorageBackendResult<()>;
}

impl SerializableSession for super::VersionedKeyValueStore {
    fn establish_serializable_snapshot(&self) -> StorageBackendResult<SerializableReadContext> {
        Self::establish_serializable_snapshot(self)
    }

    fn establish_serializable_snapshot_with(
        &self,
        options: SerializableSnapshotOptions,
        capture: &mut SerializableSnapshotCapture<'_>,
    ) -> StorageBackendResult<SerializableReadContext> {
        self.require_mutable_session()?;
        self.active
            .lock()
            .as_mut()
            .ok_or_else(super::no_transaction)?
            .establish_serializable_with(&self.persistence, options, capture, &self.write_control())
            .map_err(VersionError::into_storage_error)
    }

    fn serializable_read_context(&self) -> StorageBackendResult<Option<SerializableReadContext>> {
        if let Some(view) = &self.retained {
            return Ok(view.serializable().cloned());
        }
        let active = self.active.lock();
        let Some(transaction) = active.as_ref() else {
            return Ok(None);
        };
        transaction
            .unsealed()
            .map_err(VersionError::into_storage_error)?;
        Ok(transaction.serializable_context().cloned())
    }

    fn observe_serializable_write(
        &self,
        predicate: SerializablePredicate<'_>,
    ) -> StorageBackendResult<()> {
        self.require_mutable_session()?;
        Self::observe_serializable_write(self, predicate)
    }
}

/// A retained reader attributes observations to its original transaction, even after session refresh or nested view cloning. This does not create another participant or give the reader ownership of transaction completion.
#[derive(Clone)]
pub struct SerializableReadContext {
    pub(super) persistence: Arc<dyn VersionedPersistence>,
    pub(super) participant: SerializableParticipant,
    pub(super) memory: uqa_core::memory::MemoryBudget,
    pub(super) safe: bool,
}

impl SerializableReadContext {
    pub fn id(&self) -> SerializableTransactionId {
        self.participant.id()
    }

    /// Retain the original session's allowance while observing cancellation from the invoking reader. Cloning a context or attaching a nested reader does not grant another observation budget.
    pub fn read_control(&self, cancellation: &uqa_core::CancellationToken) -> StorageReadControl {
        StorageReadControl::new(&self.memory, cancellation)
    }

    /// Observe the logical predicate before exposing its result, including an empty, cached or index-only result. Proven safe snapshots need only the original participant's lifetime check. Physical record keys are not a substitute for logical object/row/index identities.
    pub fn observe_read(
        &self,
        predicate: SerializablePredicate<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.with_graph(control, |graph| {
            if self.safe {
                // A proven safe snapshot cannot contribute to a serialization anomaly. Retained readers still validate the original participant's lifetime.
                graph.check_active(self.id())
            } else {
                graph.observe_read(self.id(), predicate, control)
            }
        })
    }

    pub fn safe_snapshot(&self, control: &StorageReadControl) -> VersionResult<SafeSnapshot> {
        self.with_graph(control, |graph| graph.safe_snapshot(self.id(), control))
    }

    pub(super) fn with_graph<T>(
        &self,
        control: &StorageReadControl,
        operation: impl FnOnce(&mut SerializableGraph) -> VersionResult<T>,
    ) -> VersionResult<T> {
        let coordinator =
            self.persistence
                .serializable_coordinator()
                .ok_or(VersionError::InvalidEncoding(
                    "serializable session lost its coordinator",
                ))?;
        let mut operation = Some(operation);
        let mut result = None;
        coordinator.with_serializable_admission(control, &mut |graph, leases| {
            let operation = operation.take().ok_or(VersionError::InvalidEncoding(
                "serializable session operation was replayed",
            ))?;
            crate::mvcc::serializable::recover(coordinator, graph, leases, control)?;
            result = Some(operation(graph)?);
            Ok(())
        })?;
        result.ok_or(VersionError::InvalidEncoding(
            "serializable session operation was not invoked",
        ))
    }
}
