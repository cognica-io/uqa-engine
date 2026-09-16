//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction-private evaluated replacements, retained command views and undo.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryReservation};
use uqa_core::CancellationToken;

use crate::read_control::StorageReadControl;
use crate::StorageSavepointId;

use super::key::RecordKey;
use super::{PreparedRecordCommit, PreparedRecordWrite, RecordWrite, VersionError, VersionResult};

struct Change {
    write: PreparedRecordWrite,
    identity: PrivateRecordRevision,
    introduced: u64,
    undone_at: Option<u64>,
    previous_for_key: Option<usize>,
    previous_active: Option<usize>,
}

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

struct Head {
    position: Option<usize>,
    _memory: MemoryReservation,
}

struct Savepoint {
    id: StorageSavepointId,
    active: Option<usize>,
}

struct State {
    revision: u64,
    heads: BTreeMap<RecordKey, Head>,
    changes: BudgetedVec<Change>,
    active: Option<usize>,
    savepoints: BudgetedVec<Savepoint>,
}

impl State {
    fn visible(
        &self,
        position: Option<usize>,
        revision: u64,
        cancellation: &CancellationToken,
    ) -> VersionResult<Option<&PreparedRecordWrite>> {
        Ok(self
            .visible_change(position, revision, cancellation)?
            .map(|change| &change.write))
    }

    fn visible_change(
        &self,
        mut position: Option<usize>,
        revision: u64,
        cancellation: &CancellationToken,
    ) -> VersionResult<Option<&Change>> {
        while let Some(index) = position {
            cancellation.check()?;
            let change = &self.changes[index];
            if change.introduced <= revision && change.undone_at.is_none_or(|end| revision < end) {
                return Ok(Some(change));
            }
            position = change.previous_for_key;
        }
        Ok(None)
    }

    fn next_revision(&self) -> VersionResult<u64> {
        self.revision
            .checked_add(1)
            .ok_or(VersionError::PrivateRevisionExhausted)
    }

    fn savepoint_position(&self, id: StorageSavepointId) -> VersionResult<usize> {
        self.savepoints
            .iter()
            .rposition(|savepoint| savepoint.id == id)
            .ok_or(VersionError::SavepointMissing(id))
    }

    fn undo_to(&mut self, target: Option<usize>) -> VersionResult<()> {
        if self.active == target {
            return Ok(());
        }
        let revision = self.next_revision()?;
        while self.active != target {
            let change =
                &mut self.changes[self.active.expect("savepoint belongs to active history")];
            change.undone_at = Some(revision);
            self.active = change.previous_active;
        }
        self.revision = revision;
        Ok(())
    }
}

struct Owner {
    state: Mutex<State>,
    memory: MemoryBudget,
}

/// One transaction's record changes. No provider lock or native transaction is retained between operations.
///
/// Command views retain their original values even after rollback and later writes. Undo marks remain in the bounded journal until this owner and its last view are dropped; this primitive does not spill or reclaim intermediate commands.
pub struct PrivateRecordChanges {
    owner: Arc<Owner>,
}

impl PrivateRecordChanges {
    pub(super) fn write_kind(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<super::commit::RecordWriteKind>> {
        let state = self.owner.state.lock();
        let Some(head) = state.heads.get(key) else {
            return Ok(None);
        };
        Ok(state
            .visible(head.position, state.revision, control.cancellation())?
            .map(PreparedRecordWrite::kind))
    }

    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            owner: Arc::new(Owner {
                state: Mutex::new(State {
                    revision: 0,
                    heads: BTreeMap::new(),
                    changes: BudgetedVec::new(memory),
                    active: None,
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

    pub(super) fn apply_owned(
        &self,
        writes: &[PreparedRecordWrite],
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        if writes.is_empty() {
            return Ok(());
        }
        let mut state = self.owner.state.lock();
        let revision = state.next_revision()?;
        let mut inserted = BudgetedVec::new(&self.owner.memory);
        for (index, write) in writes.iter().enumerate() {
            control.cancellation().check()?;
            if let Some(head) = state.heads.get(write.key()) {
                if let Some(previous) =
                    state.visible(head.position, state.revision, control.cancellation())?
                {
                    if previous.expected() != write.expected() {
                        return Err(VersionError::WriteConflict {
                            mutation: index,
                            expected: write.expected(),
                            actual: previous.expected(),
                        });
                    }
                }
            } else {
                let memory = self
                    .owner
                    .memory
                    .reserve(std::mem::size_of::<(RecordKey, Head)>())?;
                inserted.push((
                    write.shared_key(),
                    Head {
                        position: None,
                        _memory: memory,
                    },
                ))?;
            }
        }
        state.changes.reserve(writes.len())?;
        control.cancellation().check()?;
        let identity = PrivateRecordRevision::allocate()?;
        // Every fallible reservation and validation precedes publication under this mutex.
        while let Some((key, head)) = inserted.pop() {
            state.heads.insert(key, head);
        }
        for write in writes {
            let previous_for_key = state.heads[write.key()].position;
            let previous_active = state.active;
            let position = state.changes.len();
            state
                .changes
                .push(Change {
                    write: write.clone(),
                    identity,
                    introduced: revision,
                    undone_at: None,
                    previous_for_key,
                    previous_active,
                })
                .expect("complete batch capacity was reserved");
            state
                .heads
                .get_mut(write.key())
                .expect("prepared head exists")
                .position = Some(position);
            state.active = Some(position);
        }
        state.revision = revision;
        Ok(())
    }

    pub fn has_written(&self) -> bool {
        self.owner.state.lock().active.is_some()
    }

    pub fn snapshot(&self) -> VersionResult<PrivateRecordSnapshot> {
        let memory = self
            .owner
            .memory
            .reserve(std::mem::size_of::<PrivateRecordSnapshot>())?;
        let revision = self.owner.state.lock().revision;
        Ok(PrivateRecordSnapshot {
            owner: Arc::clone(&self.owner),
            revision,
            _memory: memory,
        })
    }

    pub fn savepoint(&self, id: StorageSavepointId) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        let active = state.active;
        state.savepoints.push(Savepoint { id, active })?;
        Ok(())
    }

    /// Release the nearest matching identity and its nested savepoints, retaining every write.
    pub fn release_savepoint(&self, id: StorageSavepointId) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        let position = state.savepoint_position(id)?;
        state.savepoints.truncate(position);
        Ok(())
    }

    /// Undo only this owner's changes, retaining the target savepoint for another rollback.
    pub fn rollback_to_savepoint(&self, id: StorageSavepointId) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        let position = state.savepoint_position(id)?;
        let active = state.savepoints[position].active;
        state.undo_to(active)?;
        state.savepoints.truncate(position + 1);
        Ok(())
    }

    pub fn rollback(&self) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        state.undo_to(None)?;
        state.savepoints.clear();
        Ok(())
    }

    /// Capture immutable final replacements without reevaluating or copying their payloads.
    pub fn prepare(&self, control: &StorageReadControl) -> VersionResult<PreparedRecordCommit> {
        control.cancellation().check()?;
        let state = self.owner.state.lock();
        let mut writes = BudgetedVec::new(control.memory());
        for head in state.heads.values() {
            control.cancellation().check()?;
            if let Some(write) =
                state.visible(head.position, state.revision, control.cancellation())?
            {
                writes.push(write.clone())?;
            }
        }
        PreparedRecordCommit::from_unique_owned(writes, control)
    }
}

/// Fixed private visibility for a command or retained source cursor; tombstones remain distinguishable from an unchanged key.
pub struct PrivateRecordSnapshot {
    owner: Arc<Owner>,
    revision: u64,
    _memory: MemoryReservation,
}

impl PrivateRecordSnapshot {
    /// Retain this exact private revision without following later writes or copying its values.
    pub(super) fn try_clone(&self) -> VersionResult<Self> {
        let memory = self.owner.memory.reserve(std::mem::size_of::<Self>())?;
        Ok(Self {
            owner: Arc::clone(&self.owner),
            revision: self.revision,
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
        let state = self.owner.state.lock();
        let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
        for (key, head) in state
            .heads
            .range::<[u8], _>((std::ops::Bound::Included(start), std::ops::Bound::Unbounded))
        {
            control.check()?;
            let key = key.bytes();
            if !key.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            if let Some(change) =
                state.visible_change(head.position, self.revision, control.cancellation())?
            {
                result.push(PrivateRecordKey {
                    key: change.write.shared_key(),
                    revision: change.identity,
                })?;
                if result.len() == limit {
                    break;
                }
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
        let state = self.owner.state.lock();
        let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
        let mut entries = state
            .heads
            .range::<[u8], _>((std::ops::Bound::Included(start), std::ops::Bound::Unbounded));
        let mut next_private = || -> VersionResult<Option<&PreparedRecordWrite>> {
            for (key, head) in entries.by_ref() {
                control.cancellation().check()?;
                if !key.bytes().starts_with(prefix) {
                    return Ok(None);
                }
                if after.is_some_and(|after| key.bytes() <= after) {
                    continue;
                }
                if let Some(write) =
                    state.visible(head.position, self.revision, control.cancellation())?
                {
                    return Ok(Some(write));
                }
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
        let state = self.owner.state.lock();
        let position = state.heads.get(key).and_then(|head| head.position);
        Ok(state
            .visible(position, self.revision, control.cancellation())?
            .cloned())
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
        let state = self.owner.state.lock();
        let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
        for (key, head) in state
            .heads
            .range::<[u8], _>((std::ops::Bound::Included(start), std::ops::Bound::Unbounded))
        {
            control.cancellation().check()?;
            let key = key.bytes();
            if !key.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            if let Some(write) =
                state.visible(head.position, self.revision, control.cancellation())?
            {
                result.push(write.clone())?;
                if result.len() == limit {
                    break;
                }
            }
        }
        Ok(result)
    }
}
