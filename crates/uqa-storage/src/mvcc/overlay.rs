//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction-private evaluated replacements, retained command views and undo.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use triomphe::Arc as StrongArc;
use uqa_core::memory::{BudgetedSharedMap, BudgetedVec, MemoryBudget, MemoryReservation};

use crate::read_control::StorageReadControl;
use crate::StorageSavepointId;

use super::key::RecordKey;
use super::{PreparedRecordCommit, PreparedRecordWrite, RecordWrite, VersionError, VersionResult};

mod cursor;
pub use cursor::{PrivateRecordCursor, PrivateRecordEntry};
mod selection;
pub(super) use selection::PrivateRecordSelection;
mod retained;
mod scopes;
pub use scopes::PrivateRevisionScope;
use scopes::RevisionScopes;
pub(in crate::mvcc) mod run;
mod run_set;
mod tiers;
use retained::{RetainedSources, Sources};
use run::SpilledRunWriter;
use run_set::RunSet;
use tiers::{TieredChange, TieredCursor};

/// The resident bytes of a memory-tier change beyond its key and value, as an estimate: its tree node and entry, the shared key and value handles and their allocation headers.
const CHANGE_OVERHEAD: usize = 192;
/// The memory tier spills once the transaction's allowance is more than this fraction used.
const PRESSURE_DIVISOR: usize = 2;
/// The memory tier spills only once it holds at least this fraction of the allowance, so that memory held elsewhere does not spill a run for every write.
const RESIDENT_DIVISOR: usize = 16;

struct Change {
    write: PreparedRecordWrite,
    identity: PrivateRecordRevision,
}

type Records = BudgetedSharedMap<RecordKey, Change>;

/// Process-local identity of an evaluated private batch. Identities are never reused across transactions or undo branches; they are not durable commit sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PrivateRecordRevision(NonZeroU64);

impl PrivateRecordRevision {
    pub(in crate::mvcc) fn allocate() -> VersionResult<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        uqa_core::atomic::try_update_u64(&NEXT, Ordering::Relaxed, Ordering::Relaxed, |id| {
            id.checked_add(1)
        })
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

    /// Decode an identity written to a spill file of this process.
    fn from_u64(value: u64) -> VersionResult<Self> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(VersionError::InvalidEncoding(
                "zero private record revision",
            ))
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
    resident: usize,
    runs: RunSet,
    sources: RetainedSources,
    scopes: Option<RevisionScopes>,
    revision: Option<PrivateRecordRevision>,
}

/// The changes of one transaction in two tiers: the newest in a persistent ordered root in memory, older ones in immutable spilled runs. A key's change in memory shadows its changes in runs, and a newer run shadows an older one.
struct State {
    records: Records,
    /// The estimated bytes the memory tier holds, which decide when it spills.
    resident: usize,
    runs: RunSet,
    sources: Sources,
    scopes: Option<RevisionScopes>,
    revision: Option<PrivateRecordRevision>,
    savepoints: BudgetedVec<Savepoint>,
}

impl State {
    /// The newest change of `key`, from either tier.
    fn change(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<TieredChange<'_>>> {
        tiers::lookup(&self.records, &self.runs, key, control)
    }

    /// The smallest memory tier that spills, which is also the size of the smallest runs.
    fn least_spill(&self) -> usize {
        self.records.budget().limit() / RESIDENT_DIVISOR
    }

    /// Move the memory tier into a new run when the transaction's allowance is under pressure: more than half of it is used and the memory tier holds a share of it worth a run. The allowance measures what this tier actually holds, so changes that share their values with an earlier owner do not spill on their own. Failure leaves both tiers unchanged.
    fn make_room(&mut self, control: &StorageReadControl) -> VersionResult<()> {
        let budget = self.records.budget();
        if self.records.is_empty()
            || self.resident < self.least_spill()
            || budget.used() <= budget.limit() / PRESSURE_DIVISOR
        {
            return Ok(());
        }
        let memory = self.records.budget().clone();
        let key_bytes = self.records.iter().fold(0_u64, |bytes, (key, _)| {
            bytes.saturating_add(key.bytes().len() as u64)
        });
        let mut writer = SpilledRunWriter::new(self.records.len() as u64, key_bytes, &memory)?;
        for (_, change) in &self.records {
            writer.push(
                change.write.key(),
                change.write.expected(),
                change.write.kind(),
                change.identity,
                change.write.value(),
                control,
            )?;
        }
        let run = writer
            .finish()?
            .expect("a nonempty memory tier spills a nonempty run");
        let base = self.least_spill() as u64;
        self.runs = self.runs.with_run(run, base, &memory, control)?;
        self.records = Records::new(&memory);
        self.resident = 0;
        Ok(())
    }

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
/// Command views and savepoints share immutable ordered roots and the spilled runs beneath them. Replacements copy only their search paths; obsolete payloads are released as soon as the last referencing root or returned record is dropped. Rollback restores a root without allocating. Once more than half of the transaction's allowance is used, the memory tier moves into a run in an encrypted temporary file, so the size of a transaction is bounded by disk rather than by its allowance.
pub struct PrivateRecordChanges {
    owner: StrongArc<Owner>,
}

impl PrivateRecordChanges {
    /// An independent writable owner over the same evaluated roots. Later writes and savepoint operations do not change the original owner; unchanged records and spill runs keep their existing allocation leases.
    pub fn fork(&self) -> Self {
        let state = self.owner.state.lock();
        Self {
            owner: StrongArc::new(Owner {
                state: Mutex::new(State {
                    records: state.records.clone(),
                    resident: state.resident,
                    runs: state.runs.clone(),
                    sources: state.sources.clone(),
                    scopes: state.scopes.as_ref().map(RevisionScopes::fork),
                    revision: state.revision,
                    savepoints: BudgetedVec::new(state.records.budget()),
                }),
            }),
        }
    }

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
        let state = self.owner.state.lock();
        Ok(state.change(key, control)?.map(|change| change.kind()))
    }

    /// A mutation needs both the original condition and the prior write kind.
    /// Read that one private entry once without materializing its value.
    pub(in crate::mvcc) fn write_metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<(super::RecordMetadata, super::commit::RecordWriteKind)>> {
        control.check()?;
        let state = self.owner.state.lock();
        Ok(state
            .change(key, control)?
            .map(|change| (change.metadata(), change.kind())))
    }

    /// Reuse immutable decoded blocks across the point lookups of one evaluated
    /// batch. Releasing the scope releases its cached bytes, even when a
    /// savepoint still owns the runs. Later changes keep their ordinary lookup
    /// precedence; this scope retains no record view.
    pub(in crate::mvcc) fn read_scope(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<run::RunCacheReader>> {
        control.check()?;
        let state = self.owner.state.lock();
        let mut readers = BudgetedVec::new(control.memory());
        for run in state.runs.newest_first() {
            readers.push(run.cache_reader())?;
        }
        Ok(readers)
    }

    pub fn new(memory: &MemoryBudget) -> Self {
        Self::with_revision_scope(memory, None)
    }

    /// Adopt a sorted derived run without copying its payloads. Its writes share `revision`; subsequent changes and savepoints use the ordinary private-root lifecycle.
    pub(in crate::mvcc) fn from_spilled_run(
        run: Option<run::SpilledRun>,
        revision: PrivateRecordRevision,
        scope: Option<PrivateRevisionScope>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let changes = Self::with_revision_scope(control.memory(), scope);
        if let Some(run) = run {
            let run = Arc::new(run);
            let mut state = changes.owner.state.lock();
            if let Some(mut scopes) = state.scopes.take() {
                let mut cursor = run.cursor(std::ops::Bound::Unbounded);
                while let Some(entry) = cursor.next(control)? {
                    let key = PreparedRecordWrite::from_shared(entry.key, None, None);
                    scopes = scopes.with_writes(&[key], revision, control)?;
                }
                state.scopes = Some(scopes);
            }
            state.runs = RunSet::from_run(run);
            state.revision = Some(revision);
        }
        control.check()?;
        Ok(changes)
    }

    pub(in crate::mvcc) fn with_revision_scope(
        memory: &MemoryBudget,
        scope: Option<PrivateRevisionScope>,
    ) -> Self {
        Self {
            owner: StrongArc::new(Owner {
                state: Mutex::new(State {
                    records: Records::new(memory),
                    resident: 0,
                    runs: RunSet::empty(),
                    sources: Sources::new(memory),
                    scopes: scope.map(|scope| RevisionScopes::new(memory, scope)),
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
        let writes = prepared.resident().ok_or(VersionError::InvalidEncoding(
            "a supplied batch is in memory",
        ))?;
        self.apply_owned(writes, control)
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
            if let Some(previous) = state.change(write.key(), control)? {
                if previous.expected() != write.expected() {
                    return Err(VersionError::WriteConflict {
                        mutation: index,
                        expected: write.expected(),
                        actual: previous.expected(),
                    });
                }
            }
        }
        state.make_room(control)?;
        let identity = PrivateRecordRevision::allocate()?;
        let scopes = state
            .scopes
            .as_ref()
            .map(|scopes| scopes.with_writes(writes, identity, control))
            .transpose()?;
        let invalidates_source = writes
            .iter()
            .any(|write| state.sources.get(write.key()).is_some_and(Option::is_some));
        if writes.len() == 1 && !invalidates_source {
            let write = &writes[0];
            control.cancellation().check()?;
            let resident = replaced_resident(&state.records, state.resident, write);
            state.records.try_insert(
                write.shared_key(),
                Change {
                    write: write.clone(),
                    identity,
                },
            )?;
            state.resident = resident;
            state.scopes = scopes;
            state.revision = Some(identity);
            return Ok(());
        }
        let mut records = state.records.clone();
        let mut sources = state.sources.clone();
        let mut resident = state.resident;
        for write in writes {
            control.cancellation().check()?;
            resident = replaced_resident(&records, resident, write);
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
        state.scopes = scopes;
        state.resident = resident;
        state.revision = Some(identity);
        Ok(())
    }

    /// Copy a prepared replacement map into an independent private root. Sorted spilled writes go directly to one fresh run; failure exposes no partially staged root.
    pub(in crate::mvcc) fn from_prepared(
        prepared: &PreparedRecordCommit,
        scope: Option<PrivateRevisionScope>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        if let Some(writes) = prepared.resident() {
            let changes = Self::with_revision_scope(control.memory(), scope);
            for write in writes {
                changes.apply_owned(std::slice::from_ref(write), control)?;
            }
            return Ok(changes);
        }
        let mut builder = super::commit::PreparedWritesBuilder::like(prepared, control)?;
        let mut writes = prepared.writes();
        while let Some(write) = writes.next(control)? {
            builder.push(write, control)?;
        }
        builder.finish_changes(scope, control)
    }

    pub fn has_written(&self) -> bool {
        let state = self.owner.state.lock();
        !state.records.is_empty() || !state.runs.is_empty()
    }

    pub fn snapshot(&self) -> VersionResult<PrivateRecordSnapshot> {
        let state = self.owner.state.lock();
        let memory = state
            .records
            .budget()
            .reserve(std::mem::size_of::<PrivateRecordSnapshot>())?;
        Ok(PrivateRecordSnapshot {
            records: state.records.clone(),
            runs: state.runs.clone(),
            sources: state.sources.snapshot(),
            scopes: state
                .scopes
                .as_ref()
                .map(RevisionScopes::snapshot)
                .transpose()?,
            revision: state.revision,
            _memory: memory,
        })
    }

    pub fn savepoint(&self, id: StorageSavepointId) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        let records = state.records.clone();
        let resident = state.resident;
        let runs = state.runs.clone();
        let sources = state.sources.snapshot();
        let scopes = state.scopes.as_ref().map(RevisionScopes::fork);
        let revision = state.revision;
        state.savepoints.push(Savepoint {
            id,
            records,
            resident,
            runs,
            sources,
            scopes,
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
        state.resident = state.savepoints[position].resident;
        state.runs = state.savepoints[position].runs.clone();
        let sources = state.savepoints[position].sources.clone();
        state.sources.restore(&sources);
        state.scopes = state.savepoints[position]
            .scopes
            .as_ref()
            .map(RevisionScopes::fork);
        state.revision = state.savepoints[position].revision;
        state.truncate_savepoints(position + 1);
        Ok(())
    }

    pub fn rollback(&self) -> VersionResult<()> {
        let mut state = self.owner.state.lock();
        let memory = state.records.budget().clone();
        state.records = Records::new(&memory);
        state.resident = 0;
        state.runs = RunSet::empty();
        state.sources = Sources::new(&memory);
        state.scopes = state.scopes.as_ref().map(RevisionScopes::empty);
        state.revision = None;
        state.truncate_savepoints(0);
        Ok(())
    }

    /// Capture immutable final replacements without reevaluating or copying their payloads. Changes that spilled are merged into one spilled run of final replacements, which commits read one block at a time.
    pub fn prepare(&self, control: &StorageReadControl) -> VersionResult<PreparedRecordCommit> {
        control.cancellation().check()?;
        let state = self.owner.state.lock();
        let mut changes = TieredCursor::new(
            Some(&state.records),
            &state.runs,
            std::ops::Bound::Unbounded,
            control,
        )?;
        if state.runs.is_empty() {
            let mut writes = BudgetedVec::new(control.memory());
            writes.reserve(state.records.len())?;
            while let Some(change) = changes.next(control)? {
                writes.push(change.write(control)?)?;
            }
            return PreparedRecordCommit::from_unique_owned(writes, control);
        }
        let (entries, key_bytes) = state.runs.size();
        let key_bytes = state.records.iter().fold(key_bytes, |bytes, (key, _)| {
            bytes.saturating_add(key.bytes().len() as u64)
        });
        let mut writer = SpilledRunWriter::new(
            entries.saturating_add(state.records.len() as u64),
            key_bytes,
            control.memory(),
        )?;
        while let Some(change) = changes.next(control)? {
            match &change {
                TieredChange::Resident(resident) => writer.push(
                    resident.write.key(),
                    resident.write.expected(),
                    resident.write.kind(),
                    resident.identity,
                    resident.write.value(),
                    control,
                )?,
                TieredChange::Spilled { entry, run } => writer.push_spilled(entry, run, control)?,
            }
        }
        let run = writer.finish()?.expect("a spilled transaction has changes");
        PreparedRecordCommit::from_spilled_run(run, control)
    }
}

/// Fixed private visibility for a command or retained source cursor; tombstones remain distinguishable from an unchanged key.
pub struct PrivateRecordSnapshot {
    records: Records,
    runs: RunSet,
    sources: RetainedSources,
    scopes: Option<Box<PrivateRecordSnapshot>>,
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
        let resident = self
            .records
            .last_before::<[u8]>(bound)
            .map(|(_, change)| TieredChange::Resident(change));
        let spilled = self.runs.last_before(bound, control)?;
        // The memory tier is newer, so it wins a tie.
        let last = match (resident, spilled) {
            (Some(resident), Some(spilled)) if spilled.key() > resident.key() => Some(spilled),
            (Some(resident), _) => Some(resident),
            (None, spilled) => spilled,
        };
        last.filter(|change| change.key().starts_with(prefix))
            .map(|change| change.write(control))
            .transpose()
    }

    /// Retain this exact private revision without following later writes or copying its values.
    pub(super) fn try_clone(&self) -> VersionResult<Self> {
        let memory = self.records.budget().reserve(std::mem::size_of::<Self>())?;
        Ok(Self {
            records: self.records.clone(),
            runs: self.runs.clone(),
            sources: self.sources.clone(),
            scopes: self
                .scopes
                .as_ref()
                .map(|scopes| scopes.try_clone().map(Box::new))
                .transpose()?,
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
        let start = after
            .filter(|after| *after >= prefix)
            .map_or(std::ops::Bound::Included(prefix), std::ops::Bound::Excluded);
        let mut changes =
            TieredCursor::for_prefix(Some(&self.records), &self.runs, prefix, start, control)?;
        while let Some(change) = changes.next(control)? {
            control.check()?;
            let key = change.key();
            if !key.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            result.push(PrivateRecordKey {
                key: change.shared_key(),
                revision: change.identity(),
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
        let mut changes = self.cursor(prefix, after, control)?;
        let mut pending = changes.next(control)?;
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
            while let Some(entry) = pending.as_ref().filter(|entry| entry.key() < key) {
                running = P::private(entry, control, &mut emit)?;
                if !running {
                    return Ok(false);
                }
                pending = changes.next(control)?;
            }
            if let Some(entry) = pending.as_ref().filter(|entry| entry.key() == key) {
                running = P::private(entry, control, &mut emit)?;
                if running {
                    pending = changes.next(control)?;
                }
            } else {
                running = emit(key, record)?;
            }
            Ok(running)
        })?;
        while running {
            let Some(entry) = pending.as_ref() else {
                break;
            };
            running = P::private(entry, control, &mut emit)?;
            if running {
                pending = changes.next(control)?;
            }
        }
        Ok(())
    }

    /// Select the original revision and presence from the entry without reading a spilled value.
    pub(super) fn metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<super::RecordMetadata>> {
        Ok(self
            .revision_metadata(key, control)?
            .map(|(metadata, _)| metadata))
    }

    /// Select one private entry's presence, original committed revision and exact private identity in the same point lookup, without loading its value.
    pub(super) fn revision_metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<(super::RecordMetadata, PrivateRecordRevision)>> {
        control.cancellation().check()?;
        Ok(tiers::lookup(&self.records, &self.runs, key, control)?
            .map(|change| (change.metadata(), change.identity())))
    }

    pub fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<PreparedRecordWrite>> {
        control.cancellation().check()?;
        tiers::lookup(&self.records, &self.runs, key, control)?
            .map(|change| change.write(control))
            .transpose()
    }

    /// Visit the private replacements whose keys start with `prefix` and follow `after`, including deletions, in key order until `visit` returns false. Each replacement's value is loaded only when it is visited, so a caller can bound what it keeps by bytes.
    pub fn visit(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(&PreparedRecordWrite) -> VersionResult<bool>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        let start = after
            .filter(|after| *after >= prefix)
            .map_or(std::ops::Bound::Included(prefix), std::ops::Bound::Excluded);
        let mut changes =
            TieredCursor::for_prefix(Some(&self.records), &self.runs, prefix, start, control)?;
        while let Some(change) = changes.next(control)? {
            control.cancellation().check()?;
            let key = change.key();
            if !key.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            if !visit(&change.write(control)?)? {
                break;
            }
        }
        Ok(())
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
        let start = after
            .filter(|after| *after >= prefix)
            .map_or(std::ops::Bound::Included(prefix), std::ops::Bound::Excluded);
        let mut changes =
            TieredCursor::for_prefix(Some(&self.records), &self.runs, prefix, start, control)?;
        while let Some(change) = changes.next(control)? {
            control.cancellation().check()?;
            let key = change.key();
            if !key.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            result.push(change.write(control)?)?;
            if result.len() == limit {
                break;
            }
        }
        Ok(result)
    }
}

/// The estimated memory a change takes in the memory tier.
fn resident_bytes(write: &PreparedRecordWrite) -> usize {
    write
        .key()
        .len()
        .saturating_add(write.value().map_or(0, <[u8]>::len))
        .saturating_add(CHANGE_OVERHEAD)
}

fn replaced_resident(records: &Records, resident: usize, write: &PreparedRecordWrite) -> usize {
    let previous = records
        .get(write.key())
        .map_or(0, |change| resident_bytes(&change.write));
    resident
        .saturating_sub(previous)
        .saturating_add(resident_bytes(write))
}

#[cfg(test)]
mod tests;
