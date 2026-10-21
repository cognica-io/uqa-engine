//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact retained value lookup with bounded resident state and independent writable snapshots.

use crate::{mvcc::PrivateRecordChanges, read_control::StorageReadControl, StorageBackendResult};
use std::ops::Bound;
use uqa_core::{memory::MemoryBudget, DocId, Value};

mod records;
mod tree;
use records::Records;

/// A physical value index whose ordered nodes and value chunks use the existing encrypted private-record runs. Core ordering is unchanged; SQL operator selection belongs to the caller. Each replacement publishes a new root only after all of its writes succeed. Cloning shares immutable runs and forks subsequent updates.
pub struct RetainedValueIndex {
    records: PrivateRecordChanges,
    memory: MemoryBudget,
    tier_bytes: usize,
    root: Option<DocId>,
    len: usize,
}

impl Clone for RetainedValueIndex {
    fn clone(&self) -> Self {
        Self {
            records: self.records.fork(),
            memory: self.memory.clone(),
            tier_bytes: self.tier_bytes,
            root: self.root,
            len: self.len,
        }
    }
}

impl RetainedValueIndex {
    /// One fixed 64KiB workspace remains available for run metadata and encrypted I/O when the caller chooses an earlier spill threshold. An indivisible decoded value is separate from retained state.
    pub fn new(bytes: usize) -> Self {
        let memory = MemoryBudget::new(bytes.max(64 * 1024));
        Self {
            records: PrivateRecordChanges::with_memory_tier_limit(&memory, bytes / 4),
            memory,
            tier_bytes: bytes / 4,
            root: None,
            len: 0,
        }
    }

    /// Build a balanced tree in one ordered pass. The source has already completed capture and sorting; this constructor invokes no SQL operation. Invalid order, duplicate identities or a mismatched count publish no owner.
    pub fn from_ordered(
        bytes: usize,
        count: usize,
        source: &mut dyn FnMut() -> StorageBackendResult<Option<(DocId, Value)>>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut index = Self::new(bytes);
        let records = Records::new(&index.records, &index.memory, control);
        let mut builder = tree::Builder {
            records: &records,
            source,
            previous: None,
        };
        index.root = builder.build(count, 0)?.0;
        if (builder.source)()?.is_some() {
            return Err(records::invalid("long ordered index input"));
        }
        index.len = count;
        Ok(index)
    }

    /// An exact range count uses subtree ranks and does not visit every matching key.
    pub fn count_range(
        &self,
        low: Bound<&Value>,
        high: Bound<&Value>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<usize> {
        let records = Records::new(&self.records, &self.memory, control);
        let _scope = records.read_scope()?;
        let before = match low {
            Bound::Included(value) => tree::rank(&records, self.root, value, false)?,
            Bound::Excluded(value) => tree::rank(&records, self.root, value, true)?,
            Bound::Unbounded => 0,
        };
        let through = match high {
            Bound::Included(value) => tree::rank(&records, self.root, value, true)?,
            Bound::Excluded(value) => tree::rank(&records, self.root, value, false)?,
            Bound::Unbounded => self.len as u64,
        };
        usize::try_from(through.saturating_sub(before))
            .map_err(|_| records::invalid("range count outside address space"))
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.records = PrivateRecordChanges::with_memory_tier_limit(&self.memory, self.tier_bytes);
        self.root = None;
        self.len = 0;
    }

    pub fn contains(&self, id: DocId, control: &StorageReadControl) -> StorageBackendResult<bool> {
        let records = Records::new(&self.records, &self.memory, control);
        records
            .node_at(&records.snapshot()?, id)
            .map(|node| node.is_some())
    }

    /// Read the original representation, without SQL interpretation or reevaluation.
    pub fn get(
        &self,
        id: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Value>> {
        let records = Records::new(&self.records, &self.memory, control);
        let _scope = records.read_scope()?;
        let snapshot = records.snapshot()?;
        records.get(&snapshot, id)
    }

    /// Insert, replace or delete one identity atomically. A failed spill, encoding or cancellation leaves this index and every earlier clone unchanged.
    pub fn replace(
        &mut self,
        id: DocId,
        value: Option<&Value>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let mut next = self.clone();
        let records = Records::new(&next.records, &next.memory, control);
        let scope = records.read_scope()?;
        let old = records.get(&records.snapshot()?, id)?;
        if let Some(old) = &old {
            next.root = tree::remove(&records, next.root, id, old, 0)?;
            records.remove_value(id)?;
            next.len -= 1;
        }
        if let Some(value) = value {
            records.put_value(id, value)?;
            next.root = Some(tree::insert(&records, next.root, id, value, 0)?);
            next.len = next
                .len
                .checked_add(1)
                .ok_or_else(|| records::invalid("row count overflow"))?;
        }
        control.check()?;
        drop(scope);
        *self = next;
        Ok(())
    }

    /// Visit an exact Core value interval in value/identity order. Equal values therefore yield distinct ascending identities without sorting or copying the complete bucket.
    pub fn visit_range(
        &self,
        low: Bound<&Value>,
        high: Bound<&Value>,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(DocId) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<()> {
        let records = Records::new(&self.records, &self.memory, control);
        let _scope = records.read_scope()?;
        let snapshot = records.snapshot()?;
        tree::visit(&records, &snapshot, self.root, low, high, visit, 0).map(|_| ())
    }

    /// Physical identities are traversed independently of value order, preserving a caller's fallible SQL comparison order.
    pub fn visit_values(
        &self,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(DocId, Value) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<()> {
        let records = Records::new(&self.records, &self.memory, control);
        let _scope = records.read_scope()?;
        let snapshot = records.snapshot()?;
        records.visit_values(&snapshot, visit)
    }
}

#[cfg(test)]
mod tests;
