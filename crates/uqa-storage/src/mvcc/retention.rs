//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Snapshot admission and reclamation share one gate; retained views own their liveness independently of that gate.

mod conformance;
pub use conformance::verify_version_reclamation;

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use uqa_core::memory::MemoryReservation;

use super::{CommitSequence, VersionError, VersionResult};
use crate::read_control::StorageReadControl;

/// A typed ordered retention boundary. Record commits and autonomous metadata epochs use independent registries and cannot be interchanged by callers.
pub trait SnapshotBoundary: Copy + Ord + Send + Sync + 'static {}

impl SnapshotBoundary for CommitSequence {}
impl SnapshotBoundary for super::MetadataEpoch {}

/// Provider-owned cross-process transport. All methods except lease destruction run under the common registry's local admission gate. The native admission gate must also exclude other processes' captures and collectors. Releasing a retained lease must never wait for admission or physical persistence.
pub trait SnapshotLeaseTransport<Boundary: SnapshotBoundary = CommitSequence>: Send + Sync {
    fn acquire_admission(&self, control: &StorageReadControl) -> VersionResult<()>;
    fn release_admission(&self);
    fn retain(
        &self,
        sequence: Boundary,
        control: &StorageReadControl,
    ) -> VersionResult<Box<dyn Send + Sync>>;
    fn oldest(&self, control: &StorageReadControl) -> VersionResult<Option<Boundary>>;
}

struct Admission<'a, Boundary: SnapshotBoundary>(Option<&'a dyn SnapshotLeaseTransport<Boundary>>);

impl<Boundary: SnapshotBoundary> Drop for Admission<'_, Boundary> {
    fn drop(&mut self) {
        if let Some(transport) = self.0 {
            transport.release_admission();
        }
    }
}

/// One registry per physical database owner, shared by every logical session and retained view. Native providers with multiple process owners supply a liveness transport; an exclusively owned database needs only this local registry.
pub struct SnapshotRegistry<Boundary: SnapshotBoundary = CommitSequence> {
    admission: Mutex<()>,
    leases: Mutex<BTreeMap<SnapshotLeaseKey<Boundary>, SnapshotLeaseEntry<Boundary>>>,
    transport: Option<Arc<dyn SnapshotLeaseTransport<Boundary>>>,
}

// The allocation address distinguishes live leases at one sequence without extending their lifetime.
type SnapshotLeaseKey<Boundary> = (Boundary, usize);

struct SnapshotLeaseEntry<Boundary: SnapshotBoundary> {
    lease: Weak<SnapshotLease<Boundary>>,
    memory: MemoryReservation,
}

/// Captures of one committed sequence under the same allowance share its lease and allocation charge. Independent allowances retain separate leases, so another session cannot prolong the invoking session's reservation. Destruction never waits for the snapshot admission gate or physical persistence.
pub struct SnapshotLease<Boundary: SnapshotBoundary = CommitSequence> {
    sequence: Boundary,
    _native: Option<Box<dyn Send + Sync>>,
    registry: Arc<SnapshotRegistry<Boundary>>,
}

impl<Boundary: SnapshotBoundary> Drop for SnapshotLease<Boundary> {
    fn drop(&mut self) {
        // This mutex protects only local entries; no provider callback, admission wait or native lease operation holds it.
        self.registry
            .leases
            .lock()
            .remove(&(self.sequence, std::ptr::from_ref(self) as usize));
    }
}

impl<Boundary: SnapshotBoundary> SnapshotLease<Boundary> {
    pub fn sequence(&self) -> Boundary {
        self.sequence
    }
}

impl<Boundary: SnapshotBoundary> SnapshotRegistry<Boundary> {
    pub fn with_transport(transport: Arc<dyn SnapshotLeaseTransport<Boundary>>) -> Self {
        Self {
            admission: Mutex::new(()),
            leases: Mutex::new(BTreeMap::new()),
            transport: Some(transport),
        }
    }

    fn admitted<T>(
        &self,
        control: &StorageReadControl,
        operation: impl FnOnce() -> VersionResult<T>,
    ) -> VersionResult<T> {
        let _local_admission = loop {
            control.cancellation().check()?;
            if let Some(admission) = self.admission.try_lock() {
                break admission;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        if let Some(transport) = self.transport.as_deref() {
            transport.acquire_admission(control)?;
        }
        let _admission = Admission(self.transport.as_deref());
        operation()
    }

    /// Capture the provider's latest sequence and register its lease while collectors are excluded. The callback opens and closes its bounded physical read before returning.
    pub fn capture(
        self: &Arc<Self>,
        control: &StorageReadControl,
        read_sequence: impl FnOnce() -> VersionResult<Boundary>,
    ) -> VersionResult<Arc<SnapshotLease<Boundary>>> {
        self.admitted(control, || {
            let sequence = read_sequence()?;
            let existing = {
                let leases = self.leases.lock();
                leases
                    .range((sequence, 0)..=(sequence, usize::MAX))
                    .find_map(|(_, entry)| {
                        entry
                            .memory
                            .budget()
                            .shares_allowance(control.memory())
                            .then(|| entry.lease.upgrade())
                            .flatten()
                    })
            };
            if let Some(lease) = existing {
                return Ok(lease);
            }
            // Final lease destruction removes its entry and releases this charge without another admission.
            let memory =
                control.memory().reserve(
                    std::mem::size_of::<SnapshotLease<Boundary>>()
                        + 2 * std::mem::size_of::<usize>()
                        + std::mem::size_of::<(
                            SnapshotLeaseKey<Boundary>,
                            SnapshotLeaseEntry<Boundary>,
                        )>(),
                )?;
            let native = self
                .transport
                .as_deref()
                .map(|transport| transport.retain(sequence, control))
                .transpose()?;
            let lease = Arc::new(SnapshotLease {
                sequence,
                _native: native,
                registry: Arc::clone(self),
            });
            self.leases.lock().insert(
                (sequence, Arc::as_ptr(&lease) as usize),
                SnapshotLeaseEntry {
                    lease: Arc::downgrade(&lease),
                    memory,
                },
            );
            Ok(lease)
        })
    }

    /// Supply the oldest live boundary while excluding new captures until the provider's atomic deletion finishes. Existing snapshots and ordinary commits remain usable; disappearing leases only make this horizon conservative. Providers must keep each key's newest revision at/before the horizon, every later revision and all commit receipts.
    pub fn reclaim<T>(
        &self,
        control: &StorageReadControl,
        remove_versions: impl FnOnce(Option<Boundary>) -> VersionResult<T>,
    ) -> VersionResult<T> {
        self.admitted(control, || {
            let local = self.leases.lock().keys().next().map(|key| key.0);
            let remote = self
                .transport
                .as_deref()
                .map(|transport| transport.oldest(control))
                .transpose()?
                .flatten();
            let oldest = local.into_iter().chain(remote).min();
            remove_versions(oldest)
        })
    }
}

impl<Boundary: SnapshotBoundary> Default for SnapshotRegistry<Boundary> {
    fn default() -> Self {
        Self {
            admission: Mutex::new(()),
            leases: Mutex::new(BTreeMap::new()),
            transport: None,
        }
    }
}

/// The common retention boundary. Providers select physical predecessor keys, but may delete only revisions strictly older than each retained anchor.
#[derive(Clone, Copy)]
pub struct ReclamationHorizon(CommitSequence);

impl ReclamationHorizon {
    pub fn new(current: CommitSequence, oldest: Option<CommitSequence>) -> VersionResult<Self> {
        if oldest.is_some_and(|oldest| oldest > current) {
            return Err(VersionError::InvalidEncoding(
                "snapshot lease exceeds the committed sequence",
            ));
        }
        Ok(Self(oldest.unwrap_or(current)))
    }

    pub fn sequence(self) -> CommitSequence {
        self.0
    }

    pub fn anchor(self, revision: CommitSequence) -> VersionResult<CommitSequence> {
        if revision == CommitSequence::INITIAL || revision > self.0 {
            return Err(VersionError::InvalidEncoding(
                "reclamation anchor is outside the retained horizon",
            ));
        }
        Ok(revision)
    }
}

#[cfg(test)]
mod tests;
