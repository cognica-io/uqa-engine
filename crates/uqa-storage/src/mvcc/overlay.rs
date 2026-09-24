//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction-private evaluated replacements, retained command views and undo.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::memory::{BudgetedSharedMap, BudgetedVec, MemoryBudget, MemoryReservation};

use crate::read_control::StorageReadControl;
use crate::StorageSavepointId;

use super::key::RecordKey;
use super::{PreparedRecordCommit, PreparedRecordWrite, RecordWrite, VersionError, VersionResult};

struct Change {
    write: PreparedRecordWrite,
    identity: PrivateRecordRevision,
}

type Records = BudgetedSharedMap<RecordKey, Change>;

/// Process-local identity of an evaluated private batch. Identities are never reused across transactions or undo branches; they are not durable commit sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PrivateRecordRevision(u64);

impl PrivateRecordRevision {
    fn allocate() -> VersionResult<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map(Self)
            .map_err(|_| VersionError::PrivateRevisionExhausted)
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// A changed key and its private batch identity, without retaining or copying its payload.
pub struct PrivateRecordKey {
    key: RecordKey,
    revision: PrivateRecordRevision,
}

impl PrivateRecordKey {
    pub fn key(&self) -> &[u8] {
        self.key.bytes()
    }

    pub fn revision(&self) -> PrivateRecordRevision {
        self.revision
    }
}

struct Savepoint {
    id: StorageSavepointId,
    records: Records,
}

struct State {
    records: Records,
    savepoints: BudgetedVec<Savepoint>,
}

impl State {
    fn savepoint_position(&self, id: StorageSavepointId) -> VersionResult<usize> {
        self.savepoints
            .iter()
            .rposition(|savepoint| savepoint.id == id)
            .ok_or(VersionError::SavepointMissing(id))
    }

    fn truncate_savepoints(&mut self, len: usize) {
        if len == 0 {
            self.savepoints = BudgetedVec::new(self.records.budget());
        } else {
            self.savepoints.truncate(len);
        }
    }
}

struct Owner {
    state: Mutex<State>,
    memory: MemoryBudget,
}

/// One transaction's record changes. No provider lock or native transaction is retained between operations.
///
/// Command views and savepoints share immutable ordered roots. Replacements copy only their search paths; obsolete payloads are released as soon as the last referencing root or returned record is dropped. Rollback restores a root without allocating. This primitive does not spill retained values.
pub struct PrivateRecordChanges {
    owner: Arc<Owner>,
}

impl PrivateRecordChanges {
    pub(super) fn share_owner(&self) -> Self {
        Self {
            owner: Arc::clone(&self.owner),
        }
    }

    pub(super) fn write_kind(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<super::commit::RecordWriteKind>> {
        control.check()?;
        Ok(self
            .owner
            .state
            .lock()
            .records
            .get(key)
            .map(|change| change.write.kind()))
    }

    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            owner: Arc::new(Owner {
                state: Mutex::new(State {
                    records: Records::new(memory),
                    savepoints: BudgetedVec::new(memory),
                }),
                memory: memory.clone(),
            }),
        }
    }

    /// Stage one atomic batch of already evaluated replacements. Repeated changes to a private record must preserve its original committed precondition.
    pub fn apply(
        &self,
        writes: &[RecordWrite<'_>],
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let owned_control = StorageReadControl::new(&self.owner.memory, control.cancellation());
        let prepared = PreparedRecordCommit::new(writes, &owned_control)?;
        self.apply_owned(prepared.records(), control)
    }

    pub(crate) fn apply_owned(
        &self,
        writes: &[PreparedRecordWrite],
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        if writes.is_empty() {
            return Ok(());
        }
        let mut state = self.owner.state.lock();
        for (index, write) in writes.iter().enumerate() {
            control.cancellation().check()?;
            if let Some(previous) = state.records.get(write.key()) {
                if previous.write.expected() != write.expected() {
                    return Err(VersionError::WriteConflict {
                        mutation: index,
                        expected: write.expected(),
                        actual: previous.write.expected(),
                    });
                }
            }
        }
        let identity = PrivateRecordRevision::allocate()?;
        if let [write] = writes {
            control.cancellation().check()?;
            state.records.try_insert(
                write.shared_key(),
                Change {
                    write: write.clone(),
                    identity,
                },
            )?;
            return Ok(());
        }
        let mut records = state.records.clone();
        for write in writes {
            control.cancellation().check()?;
            records.try_insert(
                write.shared_key(),
                Change {
                    write: write.clone(),
                    identity,
                },
            )?;
        }
        control.cancellation().check()?;
        // Candidate roots own every reservation before this single atomic publication.
        state.records = records;
        Ok(())
    }

    pub fn has_written(&self) -> bool {
        !self.owner.state.lock().records.is_empty()
    }

    pub fn snapshot(&self) -> VersionResult<PrivateRecordSnapshot> {
        let memory = self
            .owner
            .memory
            .reserve(std::mem::size_of::<PrivateRecordSnapshot>())?;
        Ok(PrivateRecordSnapshot {
            records: self.owner.state.lock().records.clone(),
            _memory: memory,
        })
    }

    pub fn savepoint(&self, id: StorageSavepointId) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        let records = state.records.clone();
        state.savepoints.push(Savepoint { id, records })?;
        Ok(())
    }

    /// Release the nearest matching identity and its nested savepoints, retaining every write.
    pub fn release_savepoint(&self, id: StorageSavepointId) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        let position = state.savepoint_position(id)?;
        state.truncate_savepoints(position);
        Ok(())
    }

    /// Undo only this owner's changes, retaining the target savepoint for another rollback.
    pub fn rollback_to_savepoint(&self, id: StorageSavepointId) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        let position = state.savepoint_position(id)?;
        state.records = state.savepoints[position].records.clone();
        state.truncate_savepoints(position + 1);
        Ok(())
    }

    pub fn rollback(&self) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        state.records = Records::new(&self.owner.memory);
        state.truncate_savepoints(0);
        Ok(())
    }

    /// Capture immutable final replacements without reevaluating or copying their payloads.
    pub fn prepare(&self, control: &StorageReadControl) -> VersionResult<PreparedRecordCommit> {
        control.cancellation().check()?;
        let state = self.owner.state.lock();
        let mut writes = BudgetedVec::new(control.memory());
        for (_, change) in &state.records {
            control.cancellation().check()?;
            writes.push(change.write.clone())?;
        }
        PreparedRecordCommit::from_unique_owned(writes, control)
    }
}

/// Fixed private visibility for a command or retained source cursor; tombstones remain distinguishable from an unchanged key.
pub struct PrivateRecordSnapshot {
    records: Records,
    _memory: MemoryReservation,
}

impl PrivateRecordSnapshot {
    /// Retain this exact private revision without following later writes or copying its values.
    pub(super) fn try_clone(&self) -> VersionResult<Self> {
        let memory = self.records.budget().reserve(std::mem::size_of::<Self>())?;
        Ok(Self {
            records: self.records.clone(),
            _memory: memory,
        })
    }

    /// Select changed keys on this retained command boundary, including deletions. Undo restores their earlier identities, while later branches and other transactions receive distinct identities.
    pub fn scan_keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<PrivateRecordKey>> {
        control.check()?;
        let mut result = BudgetedVec::new(control.memory());
        if limit == 0 {
            return Ok(result);
        }
        let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
        for (key, change) in self.records.range_from(std::ops::Bound::Included(start)) {
            control.check()?;
            let key = key.bytes();
            if !key.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            result.push(PrivateRecordKey {
                key: change.write.shared_key(),
                revision: change.identity,
            })?;
            if result.len() == limit {
                break;
            }
        }
        Ok(result)
    }

    pub(super) fn visit_merged<P: super::projection::Projection>(
        &self,
        committed: &dyn super::CommittedRecordSnapshot,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut super::projection::Visitor<'_, P>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        if limit == 0 {
            return Ok(());
        }
        let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
        let mut entries = self.records.range_from(std::ops::Bound::Included(start));
        let mut next_private = || -> VersionResult<Option<&PreparedRecordWrite>> {
            for (key, change) in entries.by_ref() {
                control.cancellation().check()?;
                if !key.bytes().starts_with(prefix) {
                    return Ok(None);
                }
                if after.is_some_and(|after| key.bytes() <= after) {
                    continue;
                }
                return Ok(Some(&change.write));
            }
            Ok(None)
        };
        let mut pending = next_private()?;
        let mut count = 0;
        let mut running = true;
        let mut emit = |key: &[u8], record: P::Record<'_>| -> VersionResult<bool> {
            control.cancellation().check()?;
            count += 1;
            let more = visit(key, record)?;
            control.cancellation().check()?;
            Ok(more && count < limit)
        };
        P::visit(committed, prefix, after, control, &mut |key, record| {
            while let Some(write) = pending.filter(|write| write.key() < key) {
                running = emit(write.key(), P::private(write))?;
                if !running {
                    return Ok(false);
                }
                pending = next_private()?;
            }
            if let Some(write) = pending.filter(|write| write.key() == key) {
                running = emit(key, P::private(write))?;
                pending = next_private()?;
            } else {
                running = emit(key, record)?;
            }
            Ok(running)
        })?;
        while running {
            let Some(write) = pending else {
                break;
            };
            running = emit(write.key(), P::private(write))?;
            pending = next_private()?;
        }
        Ok(())
    }

    pub fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<PreparedRecordWrite>> {
        control.cancellation().check()?;
        Ok(self.records.get(key).map(|change| change.write.clone()))
    }

    /// Return a bounded ordered page of private replacements, including deletions.
    pub fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<PreparedRecordWrite>> {
        control.cancellation().check()?;
        let mut result = BudgetedVec::new(control.memory());
        if limit == 0 {
            return Ok(result);
        }
        let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
        for (key, change) in self.records.range_from(std::ops::Bound::Included(start)) {
            control.cancellation().check()?;
            let key = key.bytes();
            if !key.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            result.push(change.write.clone())?;
            if result.len() == limit {
                break;
            }
        }
        Ok(result)
    }
}
