//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider-selected revision summaries share the private records' retention and spill rules.

use super::{
    PreparedRecordWrite, PrivateRecordChanges, PrivateRecordKey, PrivateRecordRevision,
    PrivateRecordSnapshot, StorageReadControl, VersionError, VersionResult,
};
use triomphe::Arc;
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryReservation};

/// Select the prefix whose newest private revision summarizes a record, or omit a record that cannot invalidate the provider's caches. The mapping must be pure and stable for the session; common storage does not interpret physical keys.
pub type PrivateRevisionScope = fn(&[u8]) -> VersionResult<Option<&[u8]>>;

pub(super) struct RevisionScopes {
    classify: PrivateRevisionScope,
    changes: PrivateRecordChanges,
    empty: PrivateRecordChanges,
    owner_memory: Option<Arc<MemoryReservation>>,
}

impl RevisionScopes {
    pub(super) fn new(memory: &MemoryBudget, classify: PrivateRevisionScope) -> Self {
        let empty = PrivateRecordChanges::new(memory);
        Self {
            classify,
            changes: empty.share_owner(),
            empty,
            owner_memory: None,
        }
    }

    pub(super) fn fork(&self) -> Self {
        Self {
            classify: self.classify,
            // Published summaries are immutable; only with_writes creates a writable owner.
            changes: self.changes.share_owner(),
            empty: self.empty.share_owner(),
            owner_memory: self.owner_memory.clone(),
        }
    }

    pub(super) fn empty(&self) -> Self {
        Self {
            classify: self.classify,
            changes: self.empty.share_owner(),
            empty: self.empty.share_owner(),
            owner_memory: None,
        }
    }

    /// Prepare independently so a later record or summary failure publishes neither root.
    pub(super) fn with_writes(
        &self,
        writes: &[PreparedRecordWrite],
        revision: PrivateRecordRevision,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let memory = self.changes.owner.state.lock().records.budget().clone();
        // Distinct summary owners can survive with savepoints; charge their retained metadata as well as their keys, records and runs.
        let owner_memory = memory.reserve(
            size_of::<super::Owner>() + size_of::<MemoryReservation>() + 2 * size_of::<usize>(),
        )?;
        let next = Self {
            classify: self.classify,
            changes: self.changes.fork(),
            empty: self.empty.share_owner(),
            owner_memory: Some(Arc::new(owner_memory)),
        };
        let owned_control = StorageReadControl::new(&memory, control.cancellation());
        let value = revision.as_u64().to_be_bytes();
        for write in writes {
            control.check()?;
            if let Some(key) = (self.classify)(write.key())? {
                let write =
                    PreparedRecordWrite::copy_bytes(key, None, Some(&value), &owned_control)?;
                next.changes.apply_owned(&[write], control)?;
            }
        }
        Ok(next)
    }

    pub(super) fn snapshot(&self) -> VersionResult<Box<PrivateRecordSnapshot>> {
        self.changes.snapshot().map(Box::new)
    }
}

impl PrivateRecordSnapshot {
    pub(in crate::mvcc) fn revision_scopes(
        &self,
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<PrivateRecordKey>> {
        let Some(scopes) = &self.scopes else {
            return self.scan_keys(&[], after, limit, control);
        };
        control.check()?;
        let mut result = BudgetedVec::new(control.memory());
        if limit == 0 {
            return Ok(result);
        }
        scopes.visit(&[], after, control, &mut |write| {
            control.check()?;
            let bytes = write
                .value()
                .and_then(|value| value.try_into().ok())
                .ok_or(VersionError::InvalidEncoding(
                    "invalid private revision scope",
                ))?;
            result.push(PrivateRecordKey {
                key: write.shared_key(),
                revision: PrivateRecordRevision::from_u64(u64::from_be_bytes(bytes))?,
            })?;
            Ok(result.len() < limit)
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
