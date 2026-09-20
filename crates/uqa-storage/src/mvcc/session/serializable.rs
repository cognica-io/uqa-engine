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

/// A retained reader attributes observations to its original transaction, even after session refresh or nested view cloning. This does not create another participant or give the reader ownership of transaction completion.
#[derive(Clone)]
pub struct SerializableReadContext {
    pub(super) persistence: Arc<dyn VersionedPersistence>,
    pub(super) participant: SerializableParticipant,
}

impl SerializableReadContext {
    pub fn id(&self) -> SerializableTransactionId {
        self.participant.id()
    }

    /// Register the logical predicate before exposing its result, including an empty, cached or index-only result. Physical record keys are not a substitute for logical object/row/index identities.
    pub fn observe_read(
        &self,
        predicate: SerializablePredicate<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.with_graph(control, |graph| {
            graph.observe_read(self.id(), predicate, control)
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
