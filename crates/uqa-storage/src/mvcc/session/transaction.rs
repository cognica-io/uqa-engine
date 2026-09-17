//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::mvcc::commit::{RecordRequirement, RecordWriteKind};
use crate::mvcc::graph::OwnedGraphMutation;
use crate::mvcc::key::RecordKey;
use crate::mvcc::vector::OwnedVectorMutation;
use crate::mvcc::{
    CommitErrorOutcome, CommitFailure, CommitStatus, CommittedRecordSnapshot, MergedRecordSnapshot,
    PreparedRecordCommit, PrivateRecordChanges, RecordWrite, StorageTransactionId, VersionError,
    VersionResult, VersionedPersistence,
};
use crate::read_control::StorageReadControl;
use crate::{StorageBackendError, StorageSavepointId};

struct Savepoint {
    name: BudgetedVec<u8>,
    id: StorageSavepointId,
    graph_position: usize,
    vector_position: usize,
    requirement_position: usize,
}

pub(super) struct Transaction {
    committed: Arc<dyn CommittedRecordSnapshot>,
    pub(super) changes: PrivateRecordChanges,
    read_only: bool,
    pub(super) allocation: Option<StorageTransactionId>,
    prepared: Option<PreparedRecordCommit>,
    materialized: Option<PreparedRecordCommit>,
    graph: BudgetedVec<OwnedGraphMutation>,
    vector: BudgetedVec<OwnedVectorMutation>,
    requirements: BudgetedVec<RecordRequirement>,
    outcome: Option<CommitErrorOutcome>,
    savepoints: BudgetedVec<Savepoint>,
}

impl Transaction {
    pub(super) fn new(
        persistence: &dyn VersionedPersistence,
        read_only: bool,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        Ok(Self {
            committed: persistence.snapshot(control)?,
            changes: PrivateRecordChanges::new(control.memory()),
            read_only,
            allocation: None,
            prepared: None,
            materialized: None,
            graph: BudgetedVec::new(control.memory()),
            vector: BudgetedVec::new(control.memory()),
            requirements: BudgetedVec::new(control.memory()),
            outcome: None,
            savepoints: BudgetedVec::new(control.memory()),
        })
    }

    pub(super) fn view(&self) -> VersionResult<MergedRecordSnapshot> {
        Ok(MergedRecordSnapshot::new(
            Arc::clone(&self.committed),
            self.changes.snapshot()?,
        ))
    }

    pub(super) fn writable(&self) -> VersionResult<()> {
        if self.read_only {
            return Err(StorageBackendError::Other(
                "cannot write in a read-only KeyValue transaction".into(),
            )
            .into());
        }
        self.unsealed()
    }

    pub(super) fn unsealed(&self) -> VersionResult<()> {
        if self.prepared.is_some() {
            return Err(VersionError::TransactionSealed);
        }
        Ok(())
    }

    pub(super) fn replace(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.write_record(key, value, RecordWriteKind::Canonical, control)
    }

    pub(super) fn write_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
        kind: RecordWriteKind,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.writable()?;
        let record = self.view()?.metadata(key, control)?;
        let expected = record.and_then(|record| record.revision);
        let exists = record.is_some_and(|record| record.live);
        if value.is_none()
            && !exists
            && matches!(
                kind,
                RecordWriteKind::Canonical | RecordWriteKind::GraphPreview
            )
        {
            return Ok(());
        }
        let prepared = PreparedRecordCommit::new(
            &[RecordWrite {
                key,
                expected,
                value,
            }],
            control,
        )?;
        let kind = if kind == RecordWriteKind::GraphPreview {
            // A later preview must retain an earlier explicit replacement or canonical write to the same private record.
            self.changes.write_kind(key, control)?.unwrap_or(kind)
        } else if matches!(
            kind,
            RecordWriteKind::Occurrence
                | RecordWriteKind::OccurrenceCache
                | RecordWriteKind::IVFPreview
                | RecordWriteKind::HNSWPreview
                | RecordWriteKind::Marker
        ) && self.changes.write_kind(key, control)? == Some(RecordWriteKind::Canonical)
        {
            RecordWriteKind::Canonical
        } else {
            kind
        };
        let write = prepared.records()[0].clone().with_kind(kind);
        self.changes.apply_owned(&[write], control)
    }

    pub(super) fn graph_mutation(&mut self, mutation: &OwnedGraphMutation) -> VersionResult<()> {
        self.writable()?;
        self.graph.push(mutation.clone())?;
        Ok(())
    }

    pub(super) fn has_derived_changes(&self) -> bool {
        !self.graph.is_empty() || !self.vector.is_empty() || !self.requirements.is_empty()
    }

    pub(super) fn require_unchanged(
        &mut self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.writable()?;
        control.check()?;
        for requirement in self.requirements.iter() {
            control.cancellation().check()?;
            if requirement.key.bytes() == key {
                return Ok(());
            }
        }
        let expected = self
            .committed
            .metadata(key, control)?
            .and_then(|record| record.revision);
        self.requirements.push(RecordRequirement {
            key: RecordKey::new(key, control.memory())?,
            expected,
        })?;
        Ok(())
    }

    pub(super) fn vector_mutation(&mut self, mutation: &OwnedVectorMutation) -> VersionResult<()> {
        self.writable()?;
        self.vector.push(mutation.clone())?;
        Ok(())
    }

    pub(super) fn delete_prefix(
        &mut self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<usize> {
        self.delete_prefix_kind(prefix, RecordWriteKind::Canonical, control)
    }

    pub(super) fn fence_record(
        &mut self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.writable()?;
        let record = self.view()?.get(key, control)?;
        self.changes.apply(
            &[RecordWrite {
                key,
                expected: record
                    .as_ref()
                    .and_then(crate::mvcc::VisibleRecord::original_revision),
                value: record.as_ref().and_then(|record| record.value()),
            }],
            control,
        )
    }

    pub(super) fn delete_prefix_kind(
        &mut self,
        prefix: &[u8],
        kind: RecordWriteKind,
        control: &StorageReadControl,
    ) -> VersionResult<usize> {
        self.writable()?;
        let mut keys = BudgetedVec::new(control.memory());
        self.view()?
            .visit_keys(prefix, None, usize::MAX, control, &mut |key, record| {
                if record.live {
                    keys.push(RecordKey::new(key, control.memory())?)?;
                }
                Ok(true)
            })?;
        for key in keys.iter() {
            self.write_record(key.bytes(), None, kind, control)?;
        }
        Ok(keys.len())
    }

    pub(super) fn atomic<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> VersionResult<T>,
    ) -> VersionResult<T> {
        self.writable()?;
        let id = StorageSavepointId::allocate();
        self.changes.savepoint(id)?;
        let graph_position = self.graph.len();
        let vector_position = self.vector.len();
        let requirement_position = self.requirements.len();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self)));
        if !matches!(&result, Ok(Ok(_))) {
            self.changes.rollback_to_savepoint(id)?;
            self.graph.truncate(graph_position);
            self.vector.truncate(vector_position);
            self.requirements.truncate(requirement_position);
        }
        self.changes.release_savepoint(id)?;
        match result {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    pub(super) fn savepoint(
        &mut self,
        name: &str,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let mut owned = BudgetedVec::new(control.memory());
        owned.extend_from_slice(name.as_bytes())?;
        self.savepoints.reserve(1)?;
        let id = StorageSavepointId::allocate();
        self.changes.savepoint(id)?;
        self.savepoints.push(Savepoint {
            name: owned,
            id,
            graph_position: self.graph.len(),
            vector_position: self.vector.len(),
            requirement_position: self.requirements.len(),
        })?;
        Ok(())
    }

    fn savepoint_position(&self, name: &str) -> VersionResult<usize> {
        self.savepoints
            .iter()
            .rposition(|savepoint| &*savepoint.name == name.as_bytes())
            .ok_or_else(|| {
                StorageBackendError::Other(format!("unknown KeyValue savepoint `{name}`")).into()
            })
    }

    pub(super) fn release(&mut self, name: &str) -> VersionResult<()> {
        let position = self.savepoint_position(name)?;
        self.changes
            .release_savepoint(self.savepoints[position].id)?;
        self.savepoints.truncate(position);
        Ok(())
    }

    pub(super) fn rollback_to(&mut self, name: &str) -> VersionResult<()> {
        let position = self.savepoint_position(name)?;
        self.changes
            .rollback_to_savepoint(self.savepoints[position].id)?;
        self.graph
            .truncate(self.savepoints[position].graph_position);
        self.vector
            .truncate(self.savepoints[position].vector_position);
        self.requirements
            .truncate(self.savepoints[position].requirement_position);
        self.savepoints.truncate(position + 1);
        Ok(())
    }

    pub(super) fn commit(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> Result<(), CommitFailure> {
        match self.outcome {
            Some(CommitErrorOutcome::Committed(_)) => return Ok(()),
            Some(CommitErrorOutcome::Aborted(transaction)) => {
                return Err(VersionError::AlreadyAborted(transaction).into())
            }
            None | Some(CommitErrorOutcome::Indeterminate(_)) => {}
        }
        if self.prepared.is_none() {
            self.prepared = Some(
                self.changes
                    .prepare(control)?
                    .with_requirements(&self.requirements, control)?
                    .with_graph_effects(self.committed.sequence(), &self.graph, control)?
                    .with_vector_effects(self.committed.sequence(), &self.vector, control)?,
            );
        }
        let prepared = self.prepared.as_ref().expect("prepared once");
        if prepared.records().is_empty()
            && prepared.graph.is_none()
            && prepared.vector.is_none()
            && !prepared.has_requirements()
        {
            return Ok(());
        }
        let allocation = if let Some(id) = self.allocation {
            id
        } else {
            let id = persistence.allocate_transaction(control)?;
            self.allocation = Some(id);
            id
        };
        loop {
            if let Err(error) = self.prepare_effects(persistence, control) {
                return Err(self.retain_uncertain_outcome(allocation, error.into()));
            }
            let prepared = self
                .materialized
                .as_ref()
                .or(self.prepared.as_ref())
                .expect("prepared once");
            match persistence.commit(allocation, prepared, control) {
                Ok(receipt)
                    if receipt.transaction == allocation
                        && receipt.fingerprint == prepared.fingerprint() =>
                {
                    self.outcome = Some(CommitErrorOutcome::Committed(receipt));
                    return Ok(());
                }
                Ok(_) => {
                    self.outcome = Some(CommitErrorOutcome::Indeterminate(allocation));
                    return Err(self.retain_uncertain_outcome(
                        allocation,
                        VersionError::CommitMismatch.into(),
                    ));
                }
                Err(CommitFailure::Rejected(VersionError::CommitSnapshotChanged {
                    expected,
                    actual,
                })) if prepared.resolved_at == Some(expected) && expected != actual => {
                    // Admission proved the receipt is still pending. Re-evaluate only typed storage effects; the original user changes and fingerprint stay sealed.
                    self.materialized = None;
                }
                Err(CommitFailure::Rejected(VersionError::TransactionFinished)) => {
                    self.outcome = Some(CommitErrorOutcome::Aborted(allocation));
                    return Err(VersionError::AlreadyAborted(allocation).into());
                }
                Err(error) => return Err(self.retain_uncertain_outcome(allocation, error)),
            }
        }
    }

    fn prepare_effects(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        let prepared = self.prepared.as_ref().expect("prepared once");
        let occurrences = prepared.records().iter().any(|write| {
            matches!(
                write.kind(),
                RecordWriteKind::Occurrence | RecordWriteKind::OccurrenceCache
            )
        });
        let markers = prepared
            .records()
            .iter()
            .any(|write| write.kind() == RecordWriteKind::Marker);
        if (prepared.graph.is_some() || prepared.vector.is_some() || occurrences || markers)
            && self.materialized.is_none()
        {
            let current = persistence.snapshot(control)?;
            prepared.validate_requirements(control.cancellation(), |key| {
                Ok(current
                    .metadata(key, control)?
                    .and_then(|record| record.revision))
            })?;
            let vector = if prepared.vector.is_some() {
                Some(crate::mvcc::vector::resolve(
                    prepared,
                    &*self.committed,
                    &*current,
                    persistence,
                    control,
                )?)
            } else {
                None
            };
            let input = vector.as_ref().unwrap_or(prepared);
            let merged = if occurrences {
                Some(crate::mvcc::occurrence::resolve(
                    input,
                    &*self.committed,
                    &*current,
                    persistence.occurrence_record_layout(),
                    control,
                )?)
            } else {
                vector
            };
            let input = merged.as_ref().unwrap_or(prepared);
            let merged = if input.graph.is_some() {
                let layout =
                    persistence
                        .graph_record_layout()
                        .ok_or(VersionError::InvalidEncoding(
                            "provider has no graph record layout",
                        ))?;
                Some(crate::mvcc::graph::resolve(
                    input,
                    Arc::clone(&current),
                    layout,
                    persistence.database_id(),
                    control,
                )?)
            } else {
                merged
            };
            self.materialized = if markers {
                Some(crate::mvcc::markers::resolve(
                    merged.as_ref().unwrap_or(prepared),
                    &*current,
                    control,
                )?)
            } else {
                merged
            };
        }
        Ok(())
    }

    fn retain_uncertain_outcome(
        &mut self,
        transaction: StorageTransactionId,
        error: CommitFailure,
    ) -> CommitFailure {
        match error {
            CommitFailure::Indeterminate { source, .. } => {
                self.outcome = Some(CommitErrorOutcome::Indeterminate(transaction));
                CommitFailure::Indeterminate {
                    transaction,
                    source,
                }
            }
            CommitFailure::Rejected(error)
                if matches!(self.outcome, Some(CommitErrorOutcome::Indeterminate(_))) =>
            {
                CommitFailure::Indeterminate {
                    transaction,
                    source: error.into_storage_error(),
                }
            }
            error @ CommitFailure::Rejected(_) => error,
        }
    }

    pub(super) fn abort(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> Result<(), CommitFailure> {
        match self.outcome {
            Some(CommitErrorOutcome::Committed(receipt)) => {
                return Err(VersionError::AlreadyCommitted(receipt).into())
            }
            Some(CommitErrorOutcome::Aborted(_)) => return Ok(()),
            None | Some(CommitErrorOutcome::Indeterminate(_)) => {}
        }
        let Some(id) = self.allocation else {
            return Ok(());
        };
        let status = persistence
            .abort(id, control)
            .map_err(|error| self.retain_uncertain_outcome(id, error.into()))?;
        match status {
            CommitStatus::Aborted => {
                self.outcome = Some(CommitErrorOutcome::Aborted(id));
                Ok(())
            }
            CommitStatus::Committed(receipt) => {
                let prepared = self.prepared.as_ref().expect("allocated prepared attempt");
                if receipt.transaction != id || receipt.fingerprint != prepared.fingerprint() {
                    self.outcome = Some(CommitErrorOutcome::Indeterminate(id));
                    return Err(
                        self.retain_uncertain_outcome(id, VersionError::CommitMismatch.into())
                    );
                }
                self.outcome = Some(CommitErrorOutcome::Committed(receipt));
                Err(VersionError::AlreadyCommitted(receipt).into())
            }
            CommitStatus::Unknown | CommitStatus::Pending => {
                self.outcome = Some(CommitErrorOutcome::Indeterminate(id));
                Err(self.retain_uncertain_outcome(id, VersionError::UnknownTransaction.into()))
            }
        }
    }
}
