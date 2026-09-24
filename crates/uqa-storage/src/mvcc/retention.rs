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

/// Provider-owned cross-process transport. All methods except lease destruction run under the common registry's local admission gate. The native admission gate must also exclude other processes' captures and collectors. Releasing a retained lease must never wait for admission or physical persistence.
pub trait SnapshotLeaseTransport: Send + Sync {
    fn acquire_admission(&self, control: &StorageReadControl) -> VersionResult<()>;
    fn release_admission(&self);
    fn retain(
        &self,
        sequence: CommitSequence,
        control: &StorageReadControl,
    ) -> VersionResult<Box<dyn Send + Sync>>;
    fn oldest(&self, control: &StorageReadControl) -> VersionResult<Option<CommitSequence>>;
}

struct Admission<'a>(Option<&'a dyn SnapshotLeaseTransport>);

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if let Some(transport) = self.0 {
            transport.release_admission();
        }
    }
}

/// One registry per physical database owner, shared by every logical session and retained view. Native providers with multiple process owners supply a liveness transport; an exclusively owned database needs only this local registry.
#[derive(Default)]
pub struct SnapshotRegistry {
    leases: Mutex<BTreeMap<CommitSequence, SnapshotLeaseEntry>>,
    transport: Option<Arc<dyn SnapshotLeaseTransport>>,
}

struct SnapshotLeaseEntry {
    lease: Weak<SnapshotLease>,
    _memory: MemoryReservation,
}

/// Clones of one committed sequence share its lease and allocation charge. Destruction never waits for the snapshot admission gate, so dropping a view inside a bounded physical read cannot deadlock a collector waiting for physical write admission.
pub struct SnapshotLease {
    sequence: CommitSequence,
    _native: Option<Box<dyn Send + Sync>>,
    registry: Arc<SnapshotRegistry>,
}

impl Drop for SnapshotLease {
    fn drop(&mut self) {
        if let Some(mut leases) = self.registry.leases.try_lock() {
            if leases
                .get(&self.sequence)
                .is_some_and(|entry| entry.lease.as_ptr() == std::ptr::from_ref(self))
            {
                leases.remove(&self.sequence);
            }
        }
    }
}

impl SnapshotLease {
    pub fn sequence(&self) -> CommitSequence {
        self.sequence
    }
}

impl SnapshotRegistry {
    pub fn with_transport(transport: Arc<dyn SnapshotLeaseTransport>) -> Self {
        Self {
            leases: Mutex::new(BTreeMap::new()),
            transport: Some(transport),
        }
    }

    fn admitted<T>(
        &self,
        control: &StorageReadControl,
        operation: impl FnOnce(&mut BTreeMap<CommitSequence, SnapshotLeaseEntry>) -> VersionResult<T>,
    ) -> VersionResult<T> {
        let mut leases = loop {
            control.cancellation().check()?;
            if let Some(leases) = self.leases.try_lock() {
                break leases;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        if let Some(transport) = self.transport.as_deref() {
            transport.acquire_admission(control)?;
        }
        let _admission = Admission(self.transport.as_deref());
        leases.retain(|_, entry| entry.lease.strong_count() != 0);
        operation(&mut leases)
    }

    /// Capture the provider's latest sequence and register its lease while collectors are excluded. The callback opens and closes its bounded physical read before returning.
    pub fn capture(
        self: &Arc<Self>,
        control: &StorageReadControl,
        read_sequence: impl FnOnce() -> VersionResult<CommitSequence>,
    ) -> VersionResult<Arc<SnapshotLease>> {
        self.admitted(control, |leases| {
            let sequence = read_sequence()?;
            if let Some(lease) = leases
                .get(&sequence)
                .and_then(|entry| entry.lease.upgrade())
            {
                return Ok(lease);
            }
            // Keep the lease and registry-entry charge until the weak reference is pruned too.
            let memory = control.memory().reserve(
                std::mem::size_of::<SnapshotLease>()
                    + 2 * std::mem::size_of::<usize>()
                    + std::mem::size_of::<(CommitSequence, SnapshotLeaseEntry)>(),
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
            leases.insert(
                sequence,
                SnapshotLeaseEntry {
                    lease: Arc::downgrade(&lease),
                    _memory: memory,
                },
            );
            Ok(lease)
        })
    }

    /// Supply the oldest live boundary while excluding new captures until the provider's atomic deletion finishes. Existing snapshots and ordinary commits remain usable; disappearing leases only make this horizon conservative. Providers must keep each key's newest revision at/before the horizon, every later revision and all commit receipts.
    pub fn reclaim<T>(
        &self,
        control: &StorageReadControl,
        remove_versions: impl FnOnce(Option<CommitSequence>) -> VersionResult<T>,
    ) -> VersionResult<T> {
        self.admitted(control, |leases| {
            let local = leases.keys().next().copied();
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
