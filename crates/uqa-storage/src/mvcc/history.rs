//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered immutable revisions with one retained predecessor at the horizon.

use std::sync::Arc;
use uqa_core::memory::{BudgetedVec, MemoryBudget};

use super::{CommitSequence, VersionError, VersionResult};

pub type SharedRecordValue = Arc<BudgetedVec<u8>>;

pub struct ScannedRecord {
    pub key: BudgetedVec<u8>,
    pub version: RecordVersion<SharedRecordValue>,
}

#[derive(Debug, Clone)]
pub struct RecordVersion<T> {
    sequence: CommitSequence,
    value: Option<T>,
}

impl<T> RecordVersion<T> {
    pub fn new(sequence: CommitSequence, value: Option<T>) -> VersionResult<Self> {
        if sequence == CommitSequence::INITIAL {
            return Err(VersionError::InvalidEncoding(
                "record version has the initial sequence",
            ));
        }
        Ok(Self { sequence, value })
    }
    pub fn sequence(&self) -> CommitSequence {
        self.sequence
    }

    /// A missing payload denotes deletion; the revision itself remains present.
    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn into_parts(self) -> (CommitSequence, Option<T>) {
        (self.sequence, self.value)
    }
}

impl RecordVersion<SharedRecordValue> {
    /// Own provider bytes under the read allowance before closing a physical read window.
    pub fn copy_bytes(
        sequence: CommitSequence,
        value: Option<&[u8]>,
        control: &crate::read_control::StorageReadControl,
    ) -> VersionResult<Self> {
        control.cancellation().check()?;
        let mut record = Self::new(sequence, None)?;
        record.value = value
            .map(|bytes| {
                let mut value = BudgetedVec::new(control.memory());
                value.extend_from_slice(bytes)?;
                Ok::<_, VersionError>(Arc::new(value))
            })
            .transpose()?;
        Ok(record)
    }
}

impl ScannedRecord {
    pub fn copy_key(
        key: &[u8],
        version: RecordVersion<SharedRecordValue>,
        control: &crate::read_control::StorageReadControl,
    ) -> VersionResult<Self> {
        control.cancellation().check()?;
        let mut owned = BudgetedVec::new(control.memory());
        owned.extend_from_slice(key)?;
        Ok(Self {
            key: owned,
            version,
        })
    }
}

/// A record's committed revisions. Payload allocations retain separate owners.
#[derive(Debug)]
pub struct RecordHistory<T> {
    versions: BudgetedVec<RecordVersion<T>>,
}

impl<T> RecordHistory<T> {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            versions: BudgetedVec::new(memory),
        }
    }

    pub fn len(&self) -> usize {
        self.versions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    pub fn head(&self) -> Option<&RecordVersion<T>> {
        self.versions.last()
    }

    pub fn visible_at(&self, snapshot: CommitSequence) -> Option<&RecordVersion<T>> {
        let position = self
            .versions
            .partition_point(|version| version.sequence <= snapshot);
        position
            .checked_sub(1)
            .map(|position| &self.versions[position])
    }

    pub fn append(&mut self, sequence: CommitSequence, value: Option<T>) -> VersionResult<()> {
        self.check_sequence(sequence)?;
        self.versions.push(RecordVersion { sequence, value })?;
        Ok(())
    }

    fn check_sequence(&self, sequence: CommitSequence) -> VersionResult<()> {
        let previous = self
            .head()
            .map_or(CommitSequence::INITIAL, RecordVersion::sequence);
        if sequence <= previous {
            return Err(VersionError::CommitOrder {
                previous,
                next: sequence,
            });
        }
        Ok(())
    }

    /// Retain the newest revision at/before the horizon and every later one.
    ///
    /// The caller must hold snapshot admission while choosing and applying the horizon. Tombstones are retained as revisions, including at the head.
    ///
    /// Eligible revisions are already removed if shrinking fails; the retained versions stay valid and a later call can retry releasing excess capacity.
    pub fn reclaim_before(&mut self, horizon: CommitSequence) -> VersionResult<usize> {
        let removed = self
            .versions
            .partition_point(|version| version.sequence <= horizon)
            .saturating_sub(1);
        if removed != 0 {
            let retained = self.versions.len() - removed;
            self.versions.rotate_left(removed);
            self.versions.truncate(retained);
        }
        self.versions.shrink_to_fit()?;
        Ok(removed)
    }
}

impl<T: Clone> RecordHistory<T> {
    /// Prepare a replacement history without mutating the published owner.
    pub fn fork_appending(
        &self,
        sequence: CommitSequence,
        value: Option<T>,
    ) -> VersionResult<Self> {
        self.check_sequence(sequence)?;
        let mut candidate = Self::new(self.versions.budget());
        let capacity = self
            .versions
            .len()
            .checked_add(1)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        candidate.versions.reserve(capacity)?;
        for version in self.versions.iter() {
            candidate.versions.push(version.clone())?;
        }
        candidate.append(sequence, value)?;
        Ok(candidate)
    }
}
