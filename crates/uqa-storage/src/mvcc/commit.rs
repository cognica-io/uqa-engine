//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private evaluated changes and provider-independent revision validation.

mod builder;
mod cursor;
mod lookup;
mod metadata;
mod reclamation;
mod requirements;
mod writes;
pub(super) use builder::PreparedWritesBuilder;
pub use cursor::PreparedWriteCursor;
pub(super) use lookup::PreparedLookup;
pub use metadata::PreparedWriteMetadata;
pub(super) use requirements::RecordRequirement;
use writes::PreparedWrites;

use std::collections::BTreeMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryError};

use crate::read_control::StorageReadControl;

use super::graph::{GraphEffects, OwnedGraphMutation};
use super::key::RecordKey;
use super::{
    CommitFingerprint, CommitSequence, GraphMutation, RecordWrite, VersionError, VersionResult,
};

#[derive(Clone)]
pub struct PreparedRecordWrite {
    key: RecordKey,
    expected: Option<CommitSequence>,
    value: Option<Arc<BudgetedVec<u8>>>,
    kind: RecordWriteKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordWriteKind {
    Canonical,
    GraphCache,
    GraphPreview,
    Occurrence,
    OccurrenceCache,
    IVFPreview,
    HNSWPreview,
    Marker,
    StatisticsMaintenance,
    IdempotentDelete,
    // A value is a complete typed replacement; absence is a raw invalidation that must not leave a stale population.
    DiskANNOrigin,
    DiskANNPopulationPreview,
}

impl RecordWriteKind {
    /// The kind's code, which the commit fingerprint and spill files record.
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Canonical => 0,
            Self::GraphCache => 1,
            Self::GraphPreview => 2,
            Self::Occurrence => 3,
            Self::OccurrenceCache => 4,
            Self::IVFPreview => 5,
            Self::HNSWPreview => 6,
            Self::Marker => 7,
            Self::StatisticsMaintenance => 8,
            Self::IdempotentDelete => 9,
            Self::DiskANNOrigin => 10,
            Self::DiskANNPopulationPreview => 11,
        }
    }

    /// The kind whose code is `code`.
    pub(crate) fn from_code(code: u8) -> VersionResult<Self> {
        Ok(match code {
            0 => Self::Canonical,
            1 => Self::GraphCache,
            2 => Self::GraphPreview,
            3 => Self::Occurrence,
            4 => Self::OccurrenceCache,
            5 => Self::IVFPreview,
            6 => Self::HNSWPreview,
            7 => Self::Marker,
            8 => Self::StatisticsMaintenance,
            9 => Self::IdempotentDelete,
            10 => Self::DiskANNOrigin,
            11 => Self::DiskANNPopulationPreview,
            _ => return Err(VersionError::InvalidEncoding("unknown record write kind")),
        })
    }
}

impl PreparedRecordWrite {
    pub(super) fn from_shared(
        key: RecordKey,
        expected: Option<CommitSequence>,
        value: Option<Arc<BudgetedVec<u8>>>,
    ) -> Self {
        Self {
            key,
            expected,
            value,
            kind: RecordWriteKind::Canonical,
        }
    }

    pub(super) fn copy_bytes(
        key: &[u8],
        expected: Option<CommitSequence>,
        value: Option<&[u8]>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        control.cancellation().check()?;
        let key = RecordKey::new(key, control.memory())?;
        let value = value
            .map(|value| {
                let mut owned = BudgetedVec::new(control.memory());
                owned.extend_from_slice(value)?;
                Ok::<_, VersionError>(Arc::new(owned))
            })
            .transpose()?;
        Ok(Self::from_shared(key, expected, value))
    }

    pub fn key(&self) -> &[u8] {
        self.key.bytes()
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

    pub(super) fn shared_key(&self) -> RecordKey {
        self.key.clone()
    }

    pub(super) fn rebase(mut self, expected: Option<CommitSequence>) -> Self {
        self.expected = expected;
        self
    }

    pub(crate) fn kind(&self) -> RecordWriteKind {
        self.kind
    }
    pub(crate) fn with_kind(mut self, kind: RecordWriteKind) -> Self {
        self.kind = kind;
        self
    }

    pub(super) fn with_value(mut self, value: BudgetedVec<u8>) -> Self {
        self.value = Some(Arc::new(value));
        self
    }
}

/// Immutable prepared replacements. Construct before opening a physical writer. The writes of a transaction larger than its allowance are in a spilled run ordered by key; the others are in memory in the order they were supplied.
pub struct PreparedRecordCommit {
    writes: PreparedWrites,
    fingerprint: CommitFingerprint,
    pub(super) graph: Option<GraphEffects>,
    pub(super) vector: Option<super::vector::VectorEffects>,
    pub(super) populations: Option<super::populations::PopulationEffects>,
    pub(super) notification: Option<Arc<super::notifications::NotificationEffect>>,
    pub(super) resolved_at: Option<CommitSequence>,
    requirements: Option<Arc<BudgetedVec<RecordRequirement>>>,
    reclamation_epoch: Option<u64>,
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
            prepared.push(PreparedRecordWrite::copy_bytes(
                write.key,
                write.expected,
                write.value,
                control,
            )?)?;
        }
        control.cancellation().check()?;
        Self::from_unique_owned(prepared, control)
    }

    /// The number of writes.
    pub fn len(&self) -> usize {
        self.writes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.writes.len() == 0
    }

    /// Visit the writes in order.
    pub fn writes(&self) -> PreparedWriteCursor<'_> {
        self.writes.cursor()
    }

    /// Visit the writes of a spilled batch in key order from `start`; `None` for a batch in memory, whose writes `resident` returns in the order they were supplied.
    pub fn spilled_writes_from(&self, start: &[u8]) -> Option<PreparedWriteCursor<'_>> {
        match &self.writes {
            PreparedWrites::Resident(_) => None,
            PreparedWrites::Spilled(run) => Some(PreparedWriteCursor::spilled_from(run, start)),
        }
    }

    /// The writes, when they are in memory; a transaction larger than its allowance has spilled them.
    pub fn resident(&self) -> Option<&[PreparedRecordWrite]> {
        match &self.writes {
            PreparedWrites::Resident(writes) => Some(writes),
            PreparedWrites::Spilled(_) => None,
        }
    }

    /// Share immutable spilled writes only with a root charged to the same allowance.
    pub(super) fn shared_spilled_run(
        &self,
        memory: &MemoryBudget,
    ) -> Option<Arc<super::overlay::run::SpilledRun>> {
        match &self.writes {
            PreparedWrites::Spilled(run) if run.shares_allowance(memory) => Some(Arc::clone(run)),
            _ => None,
        }
    }

    /// Whether some write replaces `key`.
    pub(crate) fn contains_key(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<bool> {
        match &self.writes {
            PreparedWrites::Resident(writes) => Ok(writes.iter().any(|write| write.key() == key)),
            PreparedWrites::Spilled(run) => Ok(run.get(key, control)?.is_some()),
        }
    }

    /// Whether some write is not canonical: such a batch has derived effects to resolve.
    pub(crate) fn typed(&self) -> bool {
        self.writes.typed()
    }

    /// Whether some write is of `kind`.
    pub(crate) fn has_kind(&self, kind: RecordWriteKind) -> bool {
        match &self.writes {
            PreparedWrites::Resident(writes) => writes.iter().any(|write| write.kind == kind),
            PreparedWrites::Spilled(run) => run.has_kind(kind),
        }
    }

    /// The caller supplies exactly one final replacement for each identity.
    pub(super) fn from_unique_owned(
        writes: BudgetedVec<PreparedRecordWrite>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let typed = writes
            .iter()
            .any(|write| write.kind != RecordWriteKind::Canonical);
        let mut digest = fingerprint_header(typed, writes.len() as u64);
        for write in writes.iter() {
            control.cancellation().check()?;
            fingerprint_write(
                &mut digest,
                typed,
                write.kind,
                write.key(),
                write.expected(),
                write.value().map(|value| value.len() as u64),
                control,
            )?;
            if let Some(value) = write.value() {
                hash_bytes(&mut digest, value, control)?;
            }
        }
        control.cancellation().check()?;
        Ok(Self::with_fingerprint(
            PreparedWrites::Resident(writes),
            digest.finalize().into(),
        ))
    }

    /// The writes of `run`, which holds exactly one final replacement for each identity, fingerprinted as the same writes in memory are.
    pub(super) fn from_spilled_run(
        run: super::overlay::run::SpilledRun,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        Self::from_shared_spilled_run(Arc::new(run), control)
    }

    pub(super) fn from_shared_spilled_run(
        run: Arc<super::overlay::run::SpilledRun>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let typed = run.typed();
        let mut digest = fingerprint_header(typed, run.len());
        let mut cursor = run.cursor(std::ops::Bound::Unbounded);
        while let Some(entry) = cursor.next(control)? {
            fingerprint_write(
                &mut digest,
                typed,
                entry.kind,
                entry.key.bytes(),
                entry.expected,
                entry.value.map(|location| location.len),
                control,
            )?;
            if let Some(location) = entry.value {
                run.copy_value(
                    location,
                    &mut |chunk| {
                        digest.update(chunk);
                        Ok(())
                    },
                    control,
                )?;
            }
        }
        control.cancellation().check()?;
        Ok(Self::with_fingerprint(
            PreparedWrites::Spilled(run),
            digest.finalize().into(),
        ))
    }

    fn with_fingerprint(writes: PreparedWrites, fingerprint: CommitFingerprint) -> Self {
        Self {
            writes,
            fingerprint,
            graph: None,
            vector: None,
            populations: None,
            notification: None,
            resolved_at: None,
            requirements: None,
            reclamation_epoch: None,
        }
    }

    pub(super) fn with_graph_effects(
        mut self,
        base: CommitSequence,
        operations: &[OwnedGraphMutation],
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        if operations.is_empty()
            && !self.has_kind(RecordWriteKind::GraphCache)
            && !self.has_kind(RecordWriteKind::GraphPreview)
        {
            return Ok(self);
        }
        let mut owned = BudgetedVec::new(control.memory());
        owned.reserve(operations.len())?;
        let mut digest = Sha256::new();
        digest.update(b"UQA prepared graph effects 1");
        digest.update(self.fingerprint);
        digest.update(base.as_u64().to_be_bytes());
        digest.update((operations.len() as u64).to_be_bytes());
        let mut text = |bytes: &[u8]| -> VersionResult<()> {
            digest.update((bytes.len() as u64).to_be_bytes());
            hash_bytes(&mut digest, bytes, control)
        };
        for operation in operations {
            control.cancellation().check()?;
            match operation.borrowed() {
                GraphMutation::InvalidateGraph(graph) => {
                    text(b"graph")?;
                    text(graph.as_bytes())?;
                }
                GraphMutation::InvalidatePath(index) => {
                    text(b"invalidate-path")?;
                    text(index.as_bytes())?;
                }
                GraphMutation::InvalidateEntity(kind, id) => {
                    text(b"entity")?;
                    text(kind.as_str().as_bytes())?;
                    text(&id.to_be_bytes())?;
                }
                GraphMutation::PublishPath {
                    index,
                    graph,
                    definition,
                } => {
                    text(b"path")?;
                    text(index.as_bytes())?;
                    text(graph.as_bytes())?;
                    text(definition.as_bytes())?;
                }
            }
            owned.push(operation.clone())?;
        }
        self.fingerprint = digest.finalize().into();
        self.graph = Some(GraphEffects {
            base,
            operations: owned,
        });
        Ok(self)
    }

    pub(super) fn with_notification_effect(
        mut self,
        effect: Option<&Arc<super::notifications::NotificationEffect>>,
    ) -> Self {
        if let Some(effect) = effect {
            self.fingerprint = effect.seal(self.fingerprint);
            self.notification = Some(Arc::clone(effect));
        }
        self
    }

    pub(crate) fn resolved(mut self, original: &Self, sequence: CommitSequence) -> Self {
        self.fingerprint = original.fingerprint;
        self.resolved_at = Some(sequence);
        self.requirements.clone_from(&original.requirements);
        self.reclamation_epoch = original.reclamation_epoch;
        self
    }

    pub(super) fn seal_vector_effects(
        &mut self,
        fingerprint: CommitFingerprint,
        effects: super::vector::VectorEffects,
    ) {
        self.fingerprint = fingerprint;
        self.vector = Some(effects);
    }

    pub(super) fn seal_population_effects(
        &mut self,
        fingerprint: CommitFingerprint,
        effects: super::populations::PopulationEffects,
    ) {
        self.fingerprint = fingerprint;
        self.populations = Some(effects);
    }

    pub(crate) fn retain_graph_effects(
        self,
        original: &Self,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        match &original.graph {
            Some(effects) => self.with_graph_effects(effects.base, &effects.operations, control),
            None => Ok(self),
        }
    }

    /// Check the snapshot used to discover derived dependencies under exclusive admission, after resolving any existing receipt and before validating record heads. A mismatch proves this pending attempt has not published and permits pure effect preparation to restart.
    pub fn validate_snapshot(&self, current: CommitSequence) -> VersionResult<()> {
        if self.graph.is_some()
            || self.vector.is_some()
            || self.populations.is_some()
            || self.notification.is_some()
            || self.writes.typed()
        {
            return Err(VersionError::InvalidEncoding(
                "unresolved storage commit effects",
            ));
        }
        if let Some(expected) = self.resolved_at {
            if expected != current {
                return Err(VersionError::CommitSnapshotChanged {
                    expected,
                    actual: current,
                });
            }
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> CommitFingerprint {
        self.fingerprint
    }

    /// Check all preconditions under the provider's exclusive commit boundary. The callback reads current committed heads, not the caller's old snapshot.
    pub fn validate(
        &self,
        control: &StorageReadControl,
        mut head_revision: impl FnMut(&[u8]) -> VersionResult<Option<CommitSequence>>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        if self.graph.is_some()
            || self.vector.is_some()
            || self.populations.is_some()
            || self.notification.is_some()
            || self.writes.typed()
        {
            return Err(VersionError::InvalidEncoding(
                "unresolved storage commit effects",
            ));
        }
        let mut writes = self.writes();
        let mut index = 0;
        while let Some(write) = writes.next_metadata(control)? {
            let actual = head_revision(write.key())?;
            if write.expected() != actual {
                return Err(VersionError::WriteConflict {
                    mutation: index,
                    expected: write.expected(),
                    actual,
                });
            }
            index += 1;
        }
        self.validate_requirements(control.cancellation(), head_revision)?;
        control.cancellation().check()?;
        Ok(())
    }
}

fn fingerprint_header(typed: bool, len: u64) -> Sha256 {
    let mut digest = Sha256::new();
    if typed {
        digest.update(b"UQA prepared record kinds 1");
    } else {
        digest.update(b"UQA prepared records 1");
    }
    digest.update(len.to_be_bytes());
    digest
}

/// Hash one write up to its value, whose `value_len` bytes the caller hashes next.
fn fingerprint_write(
    digest: &mut Sha256,
    typed: bool,
    kind: RecordWriteKind,
    key: &[u8],
    expected: Option<CommitSequence>,
    value_len: Option<u64>,
    control: &StorageReadControl,
) -> VersionResult<()> {
    if typed {
        digest.update([kind.code()]);
    }
    digest.update((key.len() as u64).to_be_bytes());
    hash_bytes(digest, key, control)?;
    digest.update(expected.map_or(0, CommitSequence::as_u64).to_be_bytes());
    digest.update([u8::from(expected.is_some()), u8::from(value_len.is_some())]);
    if let Some(len) = value_len {
        digest.update(len.to_be_bytes());
    }
    Ok(())
}

fn hash_bytes(
    digest: &mut Sha256,
    bytes: &[u8],
    control: &StorageReadControl,
) -> VersionResult<()> {
    for chunk in bytes.chunks(65536) {
        control.cancellation().check()?;
        digest.update(chunk);
    }
    Ok(())
}
