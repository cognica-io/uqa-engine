//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory reference owner for conditional commits and pinned record reads.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryReservation};

use crate::read_control::StorageReadControl;

use super::{
    CommitSequence, PreparedRecordCommit, RecordHistory, RecordVersion, RecordWrite, VersionResult,
};

type SharedValue = Arc<BudgetedVec<u8>>;
type History = RecordHistory<SharedValue>;

#[derive(Debug)]
struct RecordKey(BudgetedVec<u8>);

impl Borrow<[u8]> for RecordKey {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

impl PartialEq for RecordKey {
    fn eq(&self, other: &Self) -> bool {
        *self.0 == *other.0
    }
}
impl Eq for RecordKey {}
impl PartialOrd for RecordKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RecordKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

struct RecordEntry {
    history: History,
    _memory: MemoryReservation,
}

#[derive(Default)]
struct State {
    sequence: CommitSequence,
    records: BTreeMap<RecordKey, RecordEntry>,
    snapshots: BTreeMap<CommitSequence, usize>,
}

struct Database {
    state: Mutex<State>,
    memory: MemoryBudget,
}

/// Reference storage for the record contract, without physical durability.
///
/// Snapshot admission, conditional validation and publication share one mutex; no lock is retained between calls. SQL execution and transaction-local overlays are deliberately outside this committed-record owner.
#[derive(Clone)]
pub struct MemoryVersionStore {
    database: Arc<Database>,
}

impl MemoryVersionStore {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            database: Arc::new(Database {
                state: Mutex::new(State::default()),
                memory: memory.clone(),
            }),
        }
    }

    pub fn snapshot(&self) -> VersionResult<MemoryRecordSnapshot> {
        let memory = self
            .database
            .memory
            .reserve(std::mem::size_of::<MemoryRecordSnapshot>())?;
        let mut state = self.database.state.lock();
        let sequence = state.sequence;
        let count = state.snapshots.entry(sequence).or_default();
        *count = count
            .checked_add(1)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        Ok(MemoryRecordSnapshot {
            database: Arc::clone(&self.database),
            sequence,
            _memory: memory,
        })
    }

    /// Validate every original revision before publishing any record.
    ///
    /// Values and replacement histories are prepared privately. Failed validation, allocation or cancellation preserves all published records.
    pub fn commit(
        &self,
        writes: &[RecordWrite<'_>],
        control: &StorageReadControl,
    ) -> VersionResult<CommitSequence> {
        let preparation = StorageReadControl::new(&self.database.memory, control.cancellation());
        let prepared = PreparedRecordCommit::new(writes, &preparation)?;
        self.commit_prepared(&prepared, control)
    }

    pub fn commit_prepared(
        &self,
        commit: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> VersionResult<CommitSequence> {
        control.cancellation().check()?;
        let mut state = self.database.state.lock();
        let writes = commit.records();
        if writes.is_empty() {
            return Ok(state.sequence);
        }
        let sequence = state.sequence.successor()?;
        commit.validate(control.cancellation(), |key| {
            Ok(state
                .records
                .get(key)
                .and_then(|entry| entry.history.head())
                .map(RecordVersion::sequence))
        })?;
        let mut prepared = BudgetedVec::new(&self.database.memory);
        prepared.reserve(writes.len())?;
        for write in writes {
            control.cancellation().check()?;
            let key = RecordKey(copy_bytes(write.key(), &self.database.memory)?);
            let value = write.shared_value();
            let history = if let Some(entry) = state.records.get(write.key()) {
                entry.history.fork_appending(sequence, value)?
            } else {
                let mut history = History::new(&self.database.memory);
                history.append(sequence, value)?;
                history
            };
            let memory = self
                .database
                .memory
                .reserve(std::mem::size_of::<(RecordKey, RecordEntry)>())?;
            prepared.push((
                key,
                RecordEntry {
                    history,
                    _memory: memory,
                },
            ))?;
        }
        control.cancellation().check()?;
        // Publication is infallible after every payload and history reservation
        // succeeds. Tree-node bookkeeping follows the collection allocator.
        while let Some((key, entry)) = prepared.pop() {
            state.records.insert(key, entry);
        }
        state.sequence = sequence;
        Ok(sequence)
    }

    /// Reclaim histories under the same gate that admits new snapshots.
    pub fn reclaim(&self) -> usize {
        let mut state = self.database.state.lock();
        let horizon = state
            .snapshots
            .first_key_value()
            .map_or(state.sequence, |(sequence, _)| *sequence);
        state
            .records
            .values_mut()
            .map(|entry| entry.history.reclaim_before(horizon))
            .sum()
    }
}

/// One pinned committed sequence. Dropping the last owner releases its lease.
pub struct MemoryRecordSnapshot {
    database: Arc<Database>,
    sequence: CommitSequence,
    _memory: MemoryReservation,
}

impl MemoryRecordSnapshot {
    pub fn sequence(&self) -> CommitSequence {
        self.sequence
    }

    /// Return a revision even if its payload is a tombstone.
    pub fn get(&self, key: &[u8]) -> Option<RecordVersion<SharedValue>> {
        self.database
            .state
            .lock()
            .records
            .get(key)
            .and_then(|entry| entry.history.visible_at(self.sequence))
            .cloned()
    }

    /// Read a bounded ordered page, including tombstones for revision checks.
    pub fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<ScannedRecord>> {
        control.cancellation().check()?;
        let mut result = BudgetedVec::new(control.memory());
        if limit == 0 {
            return Ok(result);
        }
        let state = self.database.state.lock();
        let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
        for (key, entry) in state
            .records
            .range::<[u8], _>((std::ops::Bound::Included(start), std::ops::Bound::Unbounded))
        {
            control.cancellation().check()?;
            let bytes: &[u8] = key.borrow();
            if !bytes.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| bytes <= after) {
                continue;
            }
            let Some(version) = entry.history.visible_at(self.sequence) else {
                continue;
            };
            result.push(ScannedRecord {
                key: copy_bytes(bytes, control.memory())?,
                version: version.clone(),
            })?;
            if result.len() == limit {
                break;
            }
        }
        Ok(result)
    }
}

impl Drop for MemoryRecordSnapshot {
    fn drop(&mut self) {
        let mut state = self.database.state.lock();
        let count = state
            .snapshots
            .get_mut(&self.sequence)
            .expect("snapshot owns its lease");
        *count -= 1;
        if *count == 0 {
            state.snapshots.remove(&self.sequence);
        }
    }
}

pub struct ScannedRecord {
    pub key: BudgetedVec<u8>,
    pub version: RecordVersion<SharedValue>,
}

fn copy_bytes(value: &[u8], memory: &MemoryBudget) -> VersionResult<BudgetedVec<u8>> {
    let mut owned = BudgetedVec::new(memory);
    owned.extend_from_slice(value)?;
    Ok(owned)
}
