//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider-independent reads combine one pinned committed boundary with a fixed private command view.

use std::cmp::Ordering;
use std::sync::Arc;

use uqa_core::memory::{BudgetedVec, MemoryReservation};

use crate::read_control::StorageReadControl;

use super::{
    CommitSequence, PreparedRecordWrite, PrivateRecordSnapshot, RecordVersion, ScannedRecord,
    SharedRecordValue, VersionResult,
};

/// A retained committed view whose lease protects visible versions until this owner is dropped. Provider read windows must finish inside each call, without retaining a physical writer or calling user code.
pub trait CommittedRecordSnapshot: Send + Sync {
    fn sequence(&self) -> CommitSequence;

    /// Return the newest revision at or before this snapshot, including tombstones. Missing identities return `None`.
    fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordVersion<SharedRecordValue>>>;

    /// Return at most `limit` unique keys in ascending order, strictly after `after`, including tombstones. A short page means that the prefix is exhausted at this snapshot; all pages keep the same boundary.
    fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<ScannedRecord>>;
}

struct RetainedSnapshot<T> {
    snapshot: T,
    _memory: MemoryReservation,
}

/// Charge a provider snapshot's retained metadata before allocating its shared owner. The provider snapshot itself owns any version-retention lease required by its format.
pub fn retain_record_snapshot<T: CommittedRecordSnapshot + 'static>(
    snapshot: T,
    control: &StorageReadControl,
) -> VersionResult<Arc<dyn CommittedRecordSnapshot>> {
    control.cancellation().check()?;
    let memory = control
        .memory()
        .reserve(std::mem::size_of::<RetainedSnapshot<T>>())?;
    Ok(Arc::new(RetainedSnapshot {
        snapshot,
        _memory: memory,
    }))
}

impl<T: CommittedRecordSnapshot> CommittedRecordSnapshot for RetainedSnapshot<T> {
    fn sequence(&self) -> CommitSequence {
        self.snapshot.sequence()
    }
    fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordVersion<SharedRecordValue>>> {
        self.snapshot.get(key, control)
    }
    fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<ScannedRecord>> {
        self.snapshot.scan(prefix, after, limit, control)
    }
}

/// Visible payload plus its original committed precondition. Private deletions remain observable as tombstones rather than falling back to the committed value.
pub struct VisibleRecord {
    original: Option<CommitSequence>,
    value: Option<SharedRecordValue>,
    private: bool,
}

impl VisibleRecord {
    pub fn original_revision(&self) -> Option<CommitSequence> {
        self.original
    }

    pub fn value(&self) -> Option<&[u8]> {
        self.value.as_ref().map(|value| &***value)
    }

    pub fn is_private(&self) -> bool {
        self.private
    }

    fn committed(version: RecordVersion<SharedRecordValue>) -> Self {
        let (sequence, value) = version.into_parts();
        Self {
            original: Some(sequence),
            value,
            private: false,
        }
    }

    fn private(write: &PreparedRecordWrite) -> Self {
        Self {
            original: write.expected(),
            value: write.shared_value(),
            private: true,
        }
    }
}

pub struct ScannedVisibleRecord {
    pub key: BudgetedVec<u8>,
    pub record: VisibleRecord,
}

/// Retain both boundaries across reads and paging. The transaction owner chooses when a later SQL command acquires a new committed snapshot; this reader never advances either boundary implicitly.
pub struct MergedRecordSnapshot {
    committed: Arc<dyn CommittedRecordSnapshot>,
    private: PrivateRecordSnapshot,
}

impl MergedRecordSnapshot {
    pub fn new(
        committed: Arc<dyn CommittedRecordSnapshot>,
        private: PrivateRecordSnapshot,
    ) -> Self {
        Self { committed, private }
    }

    pub fn sequence(&self) -> CommitSequence {
        self.committed.sequence()
    }

    pub fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<VisibleRecord>> {
        if let Some(write) = self.private.get(key, control)? {
            return Ok(Some(VisibleRecord::private(&write)));
        }
        Ok(self
            .committed
            .get(key, control)?
            .map(VisibleRecord::committed))
    }

    /// Merge bounded pages without duplicate identities or payload copies. Tombstones are returned so callers can preserve revision observations even when no row is visible.
    pub fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<ScannedVisibleRecord>> {
        control.cancellation().check()?;
        let mut result = BudgetedVec::new(control.memory());
        if limit == 0 {
            return Ok(result);
        }
        let (committed, _committed_memory) = self
            .committed
            .scan(prefix, after, limit, control)?
            .into_parts();
        let (private, _private_memory) = self
            .private
            .scan(prefix, after, limit, control)?
            .into_parts();
        let mut committed = committed.into_iter().peekable();
        let mut private = private.into_iter().peekable();
        while result.len() < limit {
            control.cancellation().check()?;
            let order = match (committed.peek(), private.peek()) {
                (Some(committed), Some(private)) => committed.key.as_ref().cmp(private.key()),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => break,
            };
            let record = match order {
                Ordering::Less => {
                    let committed = committed.next().expect("peeked committed record");
                    ScannedVisibleRecord {
                        key: committed.key,
                        record: VisibleRecord::committed(committed.version),
                    }
                }
                Ordering::Equal => {
                    let committed = committed.next().expect("peeked committed record");
                    let private = private.next().expect("peeked private record");
                    ScannedVisibleRecord {
                        key: committed.key,
                        record: VisibleRecord::private(&private),
                    }
                }
                Ordering::Greater => {
                    let private = private.next().expect("peeked private record");
                    let mut key = BudgetedVec::new(control.memory());
                    key.extend_from_slice(private.key())?;
                    ScannedVisibleRecord {
                        key,
                        record: VisibleRecord::private(&private),
                    }
                }
            };
            result.push(record)?;
        }
        Ok(result)
    }
}
