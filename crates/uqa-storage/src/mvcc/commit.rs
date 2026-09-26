//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private evaluated changes and provider-independent revision validation.

mod reclamation;
mod requirements;
pub(super) use requirements::RecordRequirement;

use std::collections::BTreeMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use uqa_core::memory::{BudgetedVec, MemoryError};
use uqa_core::CancellationToken;

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

/// Immutable prepared replacements. Construct before opening a physical writer.
pub struct PreparedRecordCommit {
    writes: BudgetedVec<PreparedRecordWrite>,
    fingerprint: CommitFingerprint,
    pub(super) graph: Option<GraphEffects>,
    pub(super) vector: Option<super::vector::VectorEffects>,
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

    pub fn records(&self) -> &[PreparedRecordWrite] {
        &self.writes
    }

    /// The caller supplies exactly one final replacement for each identity.
    pub(super) fn from_unique_owned(
        writes: BudgetedVec<PreparedRecordWrite>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let mut digest = Sha256::new();
        let typed = writes
            .iter()
            .any(|write| write.kind != RecordWriteKind::Canonical);
        if typed {
            digest.update(b"UQA prepared record kinds 1");
        } else {
            digest.update(b"UQA prepared records 1");
        }
        digest.update((writes.len() as u64).to_be_bytes());
        for write in writes.iter() {
            control.cancellation().check()?;
            if typed {
                digest.update([match write.kind {
                    RecordWriteKind::Canonical => 0,
                    RecordWriteKind::GraphCache => 1,
                    RecordWriteKind::GraphPreview => 2,
                    RecordWriteKind::Occurrence => 3,
                    RecordWriteKind::OccurrenceCache => 4,
                    RecordWriteKind::IVFPreview => 5,
                    RecordWriteKind::HNSWPreview => 6,
                    RecordWriteKind::Marker => 7,
                    RecordWriteKind::StatisticsMaintenance => 8,
                }]);
            }
            digest.update((write.key().len() as u64).to_be_bytes());
            hash_bytes(&mut digest, write.key(), control)?;
            digest.update(
                write
                    .expected()
                    .map_or(0, CommitSequence::as_u64)
                    .to_be_bytes(),
            );
            digest.update([
                u8::from(write.expected().is_some()),
                u8::from(write.value().is_some()),
            ]);
            if let Some(value) = write.value() {
                digest.update((value.len() as u64).to_be_bytes());
                hash_bytes(&mut digest, value, control)?;
            }
        }
        control.cancellation().check()?;
        Ok(Self {
            writes,
            fingerprint: digest.finalize().into(),
            graph: None,
            vector: None,
            notification: None,
            resolved_at: None,
            requirements: None,
            reclamation_epoch: None,
        })
    }

    pub(super) fn with_graph_effects(
        mut self,
        base: CommitSequence,
        operations: &[OwnedGraphMutation],
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        if operations.is_empty()
            && self.writes.iter().all(|write| {
                !matches!(
                    write.kind,
                    RecordWriteKind::GraphCache | RecordWriteKind::GraphPreview
                )
            })
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
            || self.notification.is_some()
            || self
                .writes
                .iter()
                .any(|write| write.kind != RecordWriteKind::Canonical)
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
        cancellation: &CancellationToken,
        mut head_revision: impl FnMut(&[u8]) -> VersionResult<Option<CommitSequence>>,
    ) -> VersionResult<()> {
        cancellation.check()?;
        if self.graph.is_some()
            || self.vector.is_some()
            || self.notification.is_some()
            || self
                .writes
                .iter()
                .any(|write| write.kind != RecordWriteKind::Canonical)
        {
            return Err(VersionError::InvalidEncoding(
                "unresolved storage commit effects",
            ));
        }
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
        self.validate_requirements(cancellation, head_revision)?;
        cancellation.check()?;
        Ok(())
    }
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
