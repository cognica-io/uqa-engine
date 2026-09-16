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

#[derive(Clone, Copy)]
pub struct BorrowedRecord<'a> {
    pub revision: Option<CommitSequence>,
    pub value: Option<&'a [u8]>,
}

/// Visible revision metadata, including tombstones, without retaining the encoded payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordMetadata {
    pub revision: Option<CommitSequence>,
    pub live: bool,
}

impl From<BorrowedRecord<'_>> for RecordMetadata {
    fn from(record: BorrowedRecord<'_>) -> Self {
        Self {
            revision: record.revision,
            live: record.value.is_some(),
        }
    }
}

pub type RecordValueVisitor<'a> = dyn FnMut(Option<BorrowedRecord<'_>>) -> VersionResult<()> + 'a;
pub type RecordScanVisitor<'a> = dyn FnMut(&[u8], BorrowedRecord<'_>) -> VersionResult<bool> + 'a;
pub type RecordKeyVisitor<'a> = dyn FnMut(&[u8], RecordMetadata) -> VersionResult<bool> + 'a;

/// A retained committed view whose lease protects visible versions until this owner is dropped. Provider read windows must finish inside each call, without retaining a physical writer or calling user code.
pub trait CommittedRecordSnapshot: Send + Sync {
    fn sequence(&self) -> CommitSequence;

    /// Read a revision and tombstone marker without materializing its value when the provider supports key-only access.
    fn metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordMetadata>> {
        let mut found = None;
        self.visit_value(key, control, &mut |record| {
            found = record.map(Into::into);
            Ok(())
        })?;
        Ok(found)
    }

    /// Visit ordered record identities, including tombstones, without fetching values from providers with key-only access.
    fn visit_keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordKeyVisitor<'_>,
    ) -> VersionResult<()> {
        self.visit_prefix(prefix, after, limit, control, &mut |key, record| {
            visit(key, record.into())
        })
    }

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

    /// Borrow provider-owned bytes for an internal storage visitor. Visitors must not reenter this snapshot or execute SQL/user callbacks.
    fn visit_value(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut RecordValueVisitor<'_>,
    ) -> VersionResult<()> {
        let record = self.get(key, control)?;
        visit(record.as_ref().map(|record| BorrowedRecord {
            revision: Some(record.sequence()),
            value: record.value().map(|value| &***value),
        }))?;
        control.cancellation().check()?;
        Ok(())
    }

    /// Visit ordered versions until the limit, exhaustion or a visitor returning `false`. Implementations with borrowed pages avoid charging their encoded payloads to the caller's decode allowance.
    fn visit_prefix(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordScanVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        let mut cursor: Option<BudgetedVec<u8>> = None;
        let mut remaining = limit;
        while remaining != 0 {
            let mut page = self.scan(
                prefix,
                cursor.as_deref().or(after),
                remaining.min(64),
                control,
            )?;
            if page.is_empty() {
                break;
            }
            for record in page.iter() {
                control.cancellation().check()?;
                if !visit(
                    &record.key,
                    BorrowedRecord {
                        revision: Some(record.version.sequence()),
                        value: record.version.value().map(|value| &***value),
                    },
                )? {
                    control.cancellation().check()?;
                    return Ok(());
                }
                remaining -= 1;
            }
            cursor = page.pop().map(|record| record.key);
        }
        control.cancellation().check()?;
        Ok(())
    }
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
    fn metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordMetadata>> {
        self.snapshot.metadata(key, control)
    }
    fn visit_keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordKeyVisitor<'_>,
    ) -> VersionResult<()> {
        self.snapshot
            .visit_keys(prefix, after, limit, control, visit)
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
    fn visit_value(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut RecordValueVisitor<'_>,
    ) -> VersionResult<()> {
        self.snapshot.visit_value(key, control, visit)
    }
    fn visit_prefix(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordScanVisitor<'_>,
    ) -> VersionResult<()> {
        self.snapshot
            .visit_prefix(prefix, after, limit, control, visit)
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
    /// Share the committed owner and retain the original private revision under its existing allowance.
    pub(crate) fn try_clone(&self) -> VersionResult<Self> {
        Ok(Self {
            committed: Arc::clone(&self.committed),
            private: self.private.try_clone()?,
        })
    }

    pub fn new(
        committed: Arc<dyn CommittedRecordSnapshot>,
        private: PrivateRecordSnapshot,
    ) -> Self {
        Self { committed, private }
    }

    pub fn sequence(&self) -> CommitSequence {
        self.committed.sequence()
    }

    /// The pinned committed boundary underlying this command view, without private replacements.
    pub fn committed(&self) -> &dyn CommittedRecordSnapshot {
        self.committed.as_ref()
    }

    /// Bounded private key metadata for provider-derived cache invalidation; values are never materialized.
    pub fn private_keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<super::PrivateRecordKey>> {
        self.private.scan_keys(prefix, after, limit, control)
    }

    pub fn visit_value(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut RecordValueVisitor<'_>,
    ) -> VersionResult<()> {
        if let Some(write) = self.private.get(key, control)? {
            visit(Some(BorrowedRecord {
                revision: write.expected(),
                value: write.value(),
            }))?;
            control.cancellation().check()?;
            return Ok(());
        }
        self.committed.visit_value(key, control, visit)
    }

    pub fn visit_prefix(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordScanVisitor<'_>,
    ) -> VersionResult<()> {
        self.private.visit_merged::<super::projection::Values>(
            &*self.committed,
            prefix,
            after,
            limit,
            control,
            visit,
        )
    }

    pub fn metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordMetadata>> {
        if let Some(write) = self.private.get(key, control)? {
            return Ok(Some(RecordMetadata {
                revision: write.expected(),
                live: write.value().is_some(),
            }));
        }
        self.committed.metadata(key, control)
    }

    pub fn visit_keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordKeyVisitor<'_>,
    ) -> VersionResult<()> {
        self.private.visit_merged::<super::projection::Keys>(
            &*self.committed,
            prefix,
            after,
            limit,
            control,
            visit,
        )
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
