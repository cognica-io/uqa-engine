//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered record edits spill before an evaluated batch becomes a transaction's private changes.

mod file;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryError};

use crate::mvcc::{
    commit::RecordWriteKind, key::RecordKey, SharedRecordValue, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;

pub(super) struct Records {
    resident: BudgetedVec<Edit>,
    spilled: Option<file::Journal>,
}

pub(super) struct Edit {
    pub(super) key: RecordKey,
    pub(super) value: Option<SharedRecordValue>,
    pub(super) kind: RecordWriteKind,
    pub(super) prefix: bool,
}

impl Edit {
    fn retain(
        key: &[u8],
        value: Option<&[u8]>,
        kind: RecordWriteKind,
        prefix: bool,
        memory: &MemoryBudget,
    ) -> VersionResult<Self> {
        let key = RecordKey::new(key, memory)?;
        let value = value
            .map(|bytes| {
                let mut owned = BudgetedVec::new(memory);
                owned.extend_from_slice(bytes)?;
                Ok::<_, MemoryError>(Arc::new(owned))
            })
            .transpose()?;
        Ok(Self {
            key,
            value,
            kind,
            prefix,
        })
    }
}

impl Records {
    pub(super) fn budget(&self) -> &MemoryBudget {
        self.resident.budget()
    }

    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            resident: BudgetedVec::new(memory),
            spilled: None,
        }
    }

    pub(super) fn push(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
        kind: RecordWriteKind,
        prefix: bool,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        let record_bytes = size_of::<Edit>()
            .checked_add(key.len())
            .and_then(|n| n.checked_add(value.map_or(0, <[u8]>::len)))
            .ok_or(MemoryError::SizeOverflow)?;
        if record_bytes > control.memory().limit() {
            return Err(MemoryError::Limit {
                required: record_bytes,
                limit: control.memory().limit(),
            }
            .into());
        }
        if self.spilled.is_none() {
            let retained =
                Edit::retain(key, value, kind, prefix, self.resident.budget()).and_then(|edit| {
                    self.resident.push(edit)?;
                    Ok(())
                });
            match retained {
                Ok(()) => return Ok(()),
                Err(VersionError::Memory(MemoryError::Limit { .. })) => self.spill(control)?,
                Err(error) => return Err(error),
            }
        }
        self.spilled
            .as_mut()
            .expect("promoted record edits")
            .push(key, value, kind, prefix, control)
    }

    fn spill(&mut self, control: &StorageReadControl) -> VersionResult<()> {
        // The batch shares one bounded resident prefix allowance across every record group.
        // Keep that prefix for zero-copy application; only the remaining edits enter the journal.
        self.spilled = Some(file::Journal::new(control)?);
        Ok(())
    }

    pub(super) fn visit(
        &self,
        control: &StorageReadControl,
        mut visit: impl FnMut(&Edit) -> VersionResult<()>,
    ) -> VersionResult<()> {
        control.check()?;
        for edit in self.resident.iter() {
            control.check()?;
            visit(edit)?;
        }
        if let Some(file) = &self.spilled {
            return file.visit(control, visit);
        }
        control.check()?;
        Ok(())
    }
}
