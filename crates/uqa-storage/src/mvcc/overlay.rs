//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction-private evaluated replacements, retained command views and undo.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use triomphe::Arc as StrongArc;
use uqa_core::memory::{BudgetedSharedMap, BudgetedVec, MemoryBudget, MemoryReservation};

use crate::read_control::StorageReadControl;
use crate::StorageSavepointId;

use super::key::RecordKey;
use super::{PreparedRecordCommit, PreparedRecordWrite, RecordWrite, VersionError, VersionResult};

mod retained;
use retained::{RetainedSources, Sources};

struct Change {
    write: PreparedRecordWrite,
    identity: PrivateRecordRevision,
}

type Records = BudgetedSharedMap<RecordKey, Change>;

/// Process-local identity of an evaluated private batch. Identities are never reused across transactions or undo branches; they are not durable commit sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PrivateRecordRevision(NonZeroU64);

impl PrivateRecordRevision {
    fn allocate() -> VersionResult<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map(|id| Self(NonZeroU64::new(id).expect("private revisions start at one")))
            .map_err(|_| VersionError::PrivateRevisionExhausted)
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::allocate().expect("test private revision")
    }

    pub fn as_u64(self) -> u64 {
        self.0.get()
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
    sources: RetainedSources,
    revision: Option<PrivateRecordRevision>,
}

struct State {
    records: Records,
    sources: Sources,
    revision: Option<PrivateRecordRevision>,
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
}

/// One transaction's record changes. No provider lock or native transaction is retained between operations.
///
/// Command views and savepoints share immutable ordered roots. Replacements copy only their search paths; obsolete payloads are released as soon as the last referencing root or returned record is dropped. Rollback restores a root without allocating. This primitive does not spill retained values.
pub struct PrivateRecordChanges {
    owner: StrongArc<Owner>,
}

impl PrivateRecordChanges {
    pub(super) fn share_owner(&self) -> Self {
        Self {
            owner: StrongArc::clone(&self.owner),
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
            owner: StrongArc::new(Owner {
                state: Mutex::new(State {
                    records: Records::new(memory),
                    sources: Sources::new(memory),
                    revision: None,
                    savepoints: BudgetedVec::new(memory),
                }),
            }),
        }
    }

    /// Stage one atomic batch of already evaluated replacements. Repeated changes to a private record must preserve its original committed precondition.
    pub fn apply(
        &self,
        writes: &[RecordWrite<'_>],
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let memory = self.owner.state.lock().records.budget().clone();
        let owned_control = StorageReadControl::new(&memory, control.cancellation());
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
        let invalidates_source = writes
            .iter()
            .any(|write| state.sources.get(write.key()).is_some_and(Option::is_some));
        if writes.len() == 1 && !invalidates_source {
            let write = &writes[0];
            control.cancellation().check()?;
            state.records.try_insert(
                write.shared_key(),
                Change {
                    write: write.clone(),
                    identity,
                },
            )?;
            state.revision = Some(identity);
            return Ok(());
        }
        let mut records = state.records.clone();
        let mut sources = state.sources.clone();
        for write in writes {
            control.cancellation().check()?;
            records.try_insert(
                write.shared_key(),
                Change {
                    write: write.clone(),
                    identity,
                },
            )?;
            if sources.get(write.key()).is_some_and(Option::is_some) {
                sources.try_insert(write.shared_key(), None)?;
            }
        }
        control.cancellation().check()?;
        // Candidate roots own every reservation before this single atomic publication.
        state.records = records;
        state.sources = sources;
        state.revision = Some(identity);
        Ok(())
    }

    pub fn has_written(&self) -> bool {
        !self.owner.state.lock().records.is_empty()
    }

    pub fn snapshot(&self) -> VersionResult<PrivateRecordSnapshot> {
        let state = self.owner.state.lock();
        let memory = state
            .records
            .budget()
            .reserve(std::mem::size_of::<PrivateRecordSnapshot>())?;
        Ok(PrivateRecordSnapshot {
            records: state.records.clone(),
            sources: state.sources.snapshot(),
            revision: state.revision,
            _memory: memory,
        })
    }

    pub fn savepoint(&self, id: StorageSavepointId) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        let records = state.records.clone();
        let sources = state.sources.snapshot();
        let revision = state.revision;
        state.savepoints.push(Savepoint {
            id,
            records,
            sources,
            revision,
        })?;
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
        let sources = state.savepoints[position].sources.clone();
        state.sources.restore(&sources);
        state.revision = state.savepoints[position].revision;
        state.truncate_savepoints(position + 1);
        Ok(())
    }

    pub fn rollback(&self) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        let memory = state.records.budget().clone();
        state.records = Records::new(&memory);
        state.sources = Sources::new(&memory);
        state.revision = None;
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
    sources: RetainedSources,
    revision: Option<PrivateRecordRevision>,
    _memory: MemoryReservation,
}

impl PrivateRecordSnapshot {
    /// Identity of this complete private root. Undo restores the saved identity; successful replacement batches receive identities never used by another root or transaction.
    pub fn revision(&self) -> Option<PrivateRecordRevision> {
        self.revision
    }

    pub(super) fn last_before(
        &self,
        prefix: &[u8],
        before: Option<&[u8]>,
        control: &StorageReadControl,
    ) -> VersionResult<Option<PreparedRecordWrite>> {
        control.check()?;
        let mut upper = BudgetedVec::new(control.memory());
        upper.extend_from_slice(prefix)?;
        let upper = match upper.iter().rposition(|byte| *byte != u8::MAX) {
            Some(last) => {
                upper[last] += 1;
                upper.truncate(last + 1);
                Some(upper)
            }
            None => None,
        };
        let end = match (before, upper.as_deref()) {
            (Some(before), Some(upper)) => Some(before.min(upper)),
            (before, upper) => before.or(upper),
        };
        let bound = end.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
        Ok(self
            .records
            .last_before::<[u8]>(bound)
            .filter(|(key, _)| key.bytes().starts_with(prefix))
            .map(|(_, change)| change.write.clone()))
    }

    /// Retain this exact private revision without following later writes or copying its values.
    pub(super) fn try_clone(&self) -> VersionResult<Self> {
        let memory = self.records.budget().reserve(std::mem::size_of::<Self>())?;
        Ok(Self {
            records: self.records.clone(),
            sources: self.sources.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_changes_keep_one_strong_counter_until_the_final_owner_drops() {
        let memory = MemoryBudget::new(1024);
        let bytes = size_of::<Owner>() + size_of::<usize>();
        let mut changes = None;
        let allocated = allocation_counter::measure(|| {
            changes = Some(PrivateRecordChanges::new(&memory));
        });
        assert_eq!(allocated.count_total, 1);
        assert_eq!(allocated.bytes_total, bytes as u64);
        let retained = changes.as_ref().unwrap().share_owner();
        let shared = allocation_counter::measure(|| drop(changes));
        assert_eq!(shared.count_total, 0);
        assert_eq!(shared.bytes_current, 0);
        let released = allocation_counter::measure(|| drop(retained));
        assert_eq!(released.count_current, -1);
        assert_eq!(released.bytes_current, -(bytes as i64));
    }
}
