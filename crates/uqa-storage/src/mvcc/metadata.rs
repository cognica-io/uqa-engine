//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Autonomous metadata generations are independent of record commit visibility.

use std::sync::Arc;
use uqa_core::memory::MemoryReservation;

use super::{CommittedRecordSnapshot, DatabaseId, VersionError, VersionResult};
use crate::read_control::StorageReadControl;

/// A borrowed identifier-table position and its allocation watermark at the captured boundary. `None` identifies internal coordination metadata or a namespace created after that boundary; it advances the caller's scan position without producing an allocation record. Returning false closes the provider read window. This callback is storage-internal and must not perform repository I/O, reenter a provider, or invoke application callbacks.
pub type IdentifierWatermarkVisitor<'a> = dyn FnMut(&[u8], Option<u64>) -> VersionResult<bool> + 'a;

/// One owner retains record visibility and autonomous allocations from the same physical publication boundary. This is an input to a provider image adapter, not a claim that its encoded catalog/index/resource image is complete.
pub trait CommittedBackupSnapshot: Send + Sync {
    fn records(&self) -> &dyn CommittedRecordSnapshot;
    fn database_id(&self) -> DatabaseId;
    fn metadata_epoch(&self) -> MetadataEpoch;

    /// Visit at most `limit` physical positions in byte order, strictly after `after`, including excluded coordinator metadata and namespaces created after capture so those keys cannot make one physical read unbounded. Only `Some` watermarks become image records. Reads finish their physical window within this call.
    fn visit_identifier_watermarks(
        &self,
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut IdentifierWatermarkVisitor<'_>,
    ) -> VersionResult<()>;
}

struct RetainedBackupSnapshot<T> {
    snapshot: T,
    _memory: MemoryReservation,
}

/// Retain one provider's joint snapshot under the invoking allowance. Record and metadata leases remain owned by that provider snapshot.
pub fn retain_backup_snapshot<T: CommittedBackupSnapshot + 'static>(
    snapshot: T,
    control: &StorageReadControl,
) -> VersionResult<Arc<dyn CommittedBackupSnapshot>> {
    control.cancellation().check()?;
    let memory = control.memory().reserve(
        std::mem::size_of::<RetainedBackupSnapshot<T>>() + 2 * std::mem::size_of::<usize>(),
    )?;
    Ok(Arc::new(RetainedBackupSnapshot {
        snapshot,
        _memory: memory,
    }))
}

impl<T: CommittedBackupSnapshot> CommittedBackupSnapshot for RetainedBackupSnapshot<T> {
    fn records(&self) -> &dyn CommittedRecordSnapshot {
        self.snapshot.records()
    }

    fn database_id(&self) -> DatabaseId {
        self.snapshot.database_id()
    }

    fn metadata_epoch(&self) -> MetadataEpoch {
        self.snapshot.metadata_epoch()
    }

    fn visit_identifier_watermarks(
        &self,
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut IdentifierWatermarkVisitor<'_>,
    ) -> VersionResult<()> {
        self.snapshot
            .visit_identifier_watermarks(after, limit, control, visit)
    }
}

/// A retained generation of autonomous metadata. Capturing a metadata boundary seals its current generation before subsequent allocations can replace it; ordinary identifier updates do not advance the record commit sequence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MetadataEpoch(u64);

impl MetadataEpoch {
    pub const INITIAL: Self = Self(0);

    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn successor(self) -> VersionResult<Self> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(VersionError::InvalidEncoding("metadata epoch exhausted"))
    }
}
