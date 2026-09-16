//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private evaluated changes and provider-independent revision validation.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::memory::{BudgetedVec, MemoryError};
use uqa_core::CancellationToken;

use crate::read_control::StorageReadControl;

use super::{CommitSequence, RecordWrite, VersionError, VersionResult};

pub struct PreparedRecordWrite {
    key: BudgetedVec<u8>,
    expected: Option<CommitSequence>,
    value: Option<Arc<BudgetedVec<u8>>>,
}

impl PreparedRecordWrite {
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn expected(&self) -> Option<CommitSequence> {
        self.expected
    }

    pub fn value(&self) -> Option<&[u8]> {
        self.value.as_ref().map(|value| &***value)
    }

    pub(crate) fn shared_value(&self) -> Option<Arc<BudgetedVec<u8>>> {
        self.value.clone()
    }
}

/// Immutable prepared replacements. Construct before opening a physical writer.
pub struct PreparedRecordCommit {
    writes: BudgetedVec<PreparedRecordWrite>,
}

impl PreparedRecordCommit {
    pub fn new(writes: &[RecordWrite<'_>], control: &StorageReadControl) -> VersionResult<Self> {
        control.cancellation().check()?;
        let slots = writes
            .len()
            .checked_mul(std::mem::size_of::<(&[u8], usize)>())
            .ok_or(MemoryError::SizeOverflow)?;
        // Charge logical tree entries; allocator node bookkeeping is separate.
        let seen_memory = control.memory().reserve(slots)?;
        let mut seen = BTreeMap::new();
        for (index, write) in writes.iter().enumerate() {
            control.cancellation().check()?;
            if let Some(first) = seen.insert(write.key, index) {
                return Err(VersionError::DuplicateRecord {
                    first,
                    second: index,
                });
            }
        }
        drop(seen);
        drop(seen_memory);

        let mut prepared = BudgetedVec::new(control.memory());
        prepared.reserve(writes.len())?;
        for write in writes {
            control.cancellation().check()?;
            let mut key = BudgetedVec::new(control.memory());
            key.extend_from_slice(write.key)?;
            let value = if let Some(value) = write.value {
                let mut owned = BudgetedVec::new(control.memory());
                owned.extend_from_slice(value)?;
                Some(Arc::new(owned))
            } else {
                None
            };
            prepared.push(PreparedRecordWrite {
                key,
                expected: write.expected,
                value,
            })?;
        }
        control.cancellation().check()?;
        Ok(Self { writes: prepared })
    }

    pub fn records(&self) -> &[PreparedRecordWrite] {
        &self.writes
    }

    /// Check all preconditions under the provider's exclusive commit boundary. The callback reads current committed heads, not the caller's old snapshot.
    pub fn validate(
        &self,
        cancellation: &CancellationToken,
        mut head_revision: impl FnMut(&[u8]) -> VersionResult<Option<CommitSequence>>,
    ) -> VersionResult<()> {
        cancellation.check()?;
        for (index, write) in self.writes.iter().enumerate() {
            cancellation.check()?;
            let actual = head_revision(write.key())?;
            if write.expected != actual {
                return Err(VersionError::WriteConflict {
                    mutation: index,
                    expected: write.expected,
                    actual,
                });
            }
        }
        cancellation.check()?;
        Ok(())
    }
}
