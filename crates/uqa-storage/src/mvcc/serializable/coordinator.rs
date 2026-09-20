//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider admission orders common participant recovery, snapshot capture and retained graph changes.

mod local;

use std::sync::Arc;

use super::{SerializableGraph, SerializableParticipant, SerializableTransactionId};
use crate::{
    mvcc::{CommittedRecordSnapshot, VersionError, VersionResult, VersionedPersistence},
    read_control::StorageReadControl,
};

pub use local::LocalSerializableState;

/// Provider-authoritative liveness while shared SSI admission is held. Lease destruction must not acquire this admission or resolve a logical transaction. Admission retains its physical owner independently of every participant.
pub trait SerializableLeases {
    fn retain(
        &self,
        id: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<SerializableParticipant>;

    fn is_alive(
        &self,
        id: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<bool>;

    fn reclaim(&self);
}

pub type SerializableOperation<'a> =
    dyn FnMut(&mut SerializableGraph, &dyn SerializableLeases) -> VersionResult<()> + 'a;

/// Shared SSI state transport paired with its authoritative physical receipts. Providers own only admission, liveness and atomic checkpoint replacement; common storage owns the conflict and recovery algorithms.
pub trait SerializableCoordinator: VersionedPersistence {
    /// Invoke the operation exactly once after loading retained state under exclusive SSI admission. Retain its graph mutations before releasing admission, including when the operation fails or cancels after confirming physical outcomes. Persistence failure takes precedence and never proves a main transaction aborted. Persist a prepared physical receipt binding in an earlier admission before publishing main records; reacquire admission for publication and outcome resolution. The operation may perform bounded main-record operations, but must not reenter SSI admission, wait for logical locks or execute application callbacks. Retain no main physical writer across the operation.
    fn with_serializable_admission(
        &self,
        control: &StorageReadControl,
        operation: &mut SerializableOperation<'_>,
    ) -> VersionResult<()>;

    /// Admit the original participant and its fixed committed snapshot at one shared boundary. Every nested or pinned view must retain this participant. This low-level capability requires all publishers to use the coordinated observation/publication protocol before SQL SSI can be enabled.
    fn admit_serializable_snapshot(
        &self,
        read_only: bool,
        control: &StorageReadControl,
    ) -> VersionResult<(SerializableParticipant, Arc<dyn CommittedRecordSnapshot>)> {
        admit_serializable(self, read_only, control, || self.snapshot(control))
    }

    /// Reconcile exact physical receipts and recover only participants whose lease owner is authoritatively gone. Unknown receipts block recovery rather than implying abort.
    fn recover_serializable_participants(&self, control: &StorageReadControl) -> VersionResult<()> {
        self.with_serializable_admission(control, &mut |graph, leases| {
            recover(self, graph, leases, control)
        })
    }
}

/// Capture an owner-specific fixed view once, after common recovery and retained participant admission. The provider retains the resulting allocation even when capture fails; no application callback or transaction work is replayed.
pub fn admit_serializable<C: SerializableCoordinator + ?Sized, T>(
    coordinator: &C,
    read_only: bool,
    control: &StorageReadControl,
    capture: impl FnOnce() -> VersionResult<T>,
) -> VersionResult<(SerializableParticipant, T)> {
    let mut capture = Some(capture);
    let mut result = None;
    coordinator.with_serializable_admission(control, &mut |graph, leases| {
        let capture = capture.take().ok_or(VersionError::InvalidEncoding(
            "serializable admission operation was replayed",
        ))?;
        recover(coordinator, graph, leases, control)?;
        let participant =
            graph.admit_with_lease(read_only, control, |id| leases.retain(id, control))?;
        let view = capture()?;
        result = Some((participant, view));
        Ok(())
    })?;
    result.ok_or(VersionError::InvalidEncoding(
        "serializable admission operation was not invoked",
    ))
}

fn recover<C: SerializableCoordinator + ?Sized>(
    coordinator: &C,
    graph: &mut SerializableGraph,
    leases: &dyn SerializableLeases,
    control: &StorageReadControl,
) -> VersionResult<()> {
    graph.reconcile_publications(control, |transaction| {
        coordinator.commit_status(transaction, control)
    })?;
    graph.recover_abandoned(
        control,
        |id| leases.is_alive(id, control),
        |transaction| coordinator.abort(transaction, control),
    )?;
    graph.reclaim();
    leases.reclaim();
    Ok(())
}
