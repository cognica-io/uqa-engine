//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

mod origin;
mod refresh;
mod serializable;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::mvcc::commit::{RecordRequirement, RecordWriteKind};
use crate::mvcc::graph::OwnedGraphMutation;
use crate::mvcc::key::RecordKey;
use crate::mvcc::vector::OwnedVectorMutation;
use crate::mvcc::{
    CommitErrorOutcome, CommitFailure, CommitSequence, CommitStatus, CommittedRecordSnapshot,
    MergedRecordSnapshot, PreparedRecordCommit, PreparedRecordWrite, PrivateRecordChanges,
    RecordWrite, SharedRecordValue, StorageTransactionId, VersionError, VersionResult,
    VersionedPersistence,
};
use crate::read_control::StorageReadControl;
use crate::{StorageBackendError, StorageSavepointId};

struct Savepoint {
    name: BudgetedVec<u8>,
    id: StorageSavepointId,
    graph_position: usize,
    vector_position: usize,
    requirement_position: usize,
    committed: Arc<dyn CommittedRecordSnapshot>,
    changes: PrivateRecordChanges,
    serializable: Option<crate::mvcc::SerializableWriteMark>,
    notification: Option<Arc<crate::mvcc::notifications::NotificationEffect>>,
}

pub(super) struct Transaction {
    committed: Arc<dyn CommittedRecordSnapshot>,
    pub(super) changes: PrivateRecordChanges,
    read_only: bool,
    pub(super) allocation: Option<StorageTransactionId>,
    receipt_owner: Option<crate::mvcc::RetainedTransactionAllocation>,
    mutation_revision: u64,
    abort_only: bool,
    prepared: Option<PreparedRecordCommit>,
    materialized: Option<PreparedRecordCommit>,
    graph: BudgetedVec<OwnedGraphMutation>,
    vector: BudgetedVec<OwnedVectorMutation>,
    requirements: BudgetedVec<RecordRequirement>,
    outcome: Option<CommitErrorOutcome>,
    savepoints: BudgetedVec<Savepoint>,
    serializable: Option<super::SerializableReadContext>,
    completion: Option<crate::mvcc::TransactionOutcome>,
    notification: Option<Arc<crate::mvcc::notifications::NotificationEffect>>,
}

impl Transaction {
    pub(super) fn new(
        persistence: &dyn VersionedPersistence,
        read_only: bool,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        Ok(Self::at_snapshot(
            persistence.snapshot(control)?,
            read_only,
            control,
        ))
    }

    pub(super) fn at_snapshot(
        committed: Arc<dyn CommittedRecordSnapshot>,
        read_only: bool,
        control: &StorageReadControl,
    ) -> Self {
        Self {
            committed,
            changes: PrivateRecordChanges::new(control.memory()),
            read_only,
            allocation: None,
            receipt_owner: None,
            mutation_revision: 0,
            abort_only: false,
            prepared: None,
            materialized: None,
            graph: BudgetedVec::new(control.memory()),
            vector: BudgetedVec::new(control.memory()),
            requirements: BudgetedVec::new(control.memory()),
            outcome: None,
            savepoints: BudgetedVec::new(control.memory()),
            serializable: None,
            completion: None,
            notification: None,
        }
    }

    pub(super) fn stage_notification(
        &mut self,
        effect: Arc<crate::mvcc::notifications::NotificationEffect>,
    ) -> VersionResult<()> {
        if self
            .notification
            .as_ref()
            .is_some_and(|current| current.same_publication(&effect))
        {
            return Ok(());
        }
        self.unsealed()?;
        self.notification = Some(effect);
        Ok(())
    }

    pub(super) fn view(&self) -> VersionResult<MergedRecordSnapshot> {
        Ok(
            MergedRecordSnapshot::new(Arc::clone(&self.committed), self.changes.snapshot()?)
                .with_serializable(self.serializable.clone()),
        )
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
        if self.prepared.is_some()
            || self.completion.is_some()
            || self.outcome.is_some()
            || self.abort_only
        {
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
        let Some((expected, kind)) = self.record_condition(key, value.is_none(), kind, control)?
        else {
            return Ok(());
        };
        let write = PreparedRecordWrite::copy_bytes(key, expected, value, control)?.with_kind(kind);
        self.changes.apply_owned(&[write], control)
    }

    pub(super) fn write_shared_record(
        &mut self,
        key: &RecordKey,
        value: Option<&SharedRecordValue>,
        kind: RecordWriteKind,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let Some((expected, kind)) =
            self.record_condition(key.bytes(), value.is_none(), kind, control)?
        else {
            return Ok(());
        };
        let write =
            PreparedRecordWrite::from_shared(key.clone(), expected, value.cloned()).with_kind(kind);
        self.changes.apply_owned(&[write], control)
    }

    fn record_condition(
        &self,
        key: &[u8],
        deleted: bool,
        kind: RecordWriteKind,
        control: &StorageReadControl,
    ) -> VersionResult<Option<(Option<CommitSequence>, RecordWriteKind)>> {
        self.writable()?;
        let record = self.view()?.metadata(key, control)?;
        let expected = record.and_then(|record| record.revision);
        let exists = record.is_some_and(|record| record.live);
        if deleted
            && !exists
            && matches!(
                kind,
                RecordWriteKind::Canonical | RecordWriteKind::GraphPreview
            )
        {
            return Ok(None);
        }
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
                | RecordWriteKind::StatisticsMaintenance
        ) && self.changes.write_kind(key, control)? == Some(RecordWriteKind::Canonical)
        {
            RecordWriteKind::Canonical
        } else {
            kind
        };
        Ok(Some((expected, kind)))
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
        let notification = self.notification.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self)));
        if !matches!(&result, Ok(Ok(_))) {
            self.changes.rollback_to_savepoint(id)?;
            truncate_retained(&mut self.graph, graph_position);
            truncate_retained(&mut self.vector, vector_position);
            truncate_retained(&mut self.requirements, requirement_position);
            self.notification = notification;
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
        let serializable = self.serializable_mark(control)?;
        self.changes.savepoint(id)?;
        self.savepoints.push(Savepoint {
            name: owned,
            id,
            graph_position: self.graph.len(),
            vector_position: self.vector.len(),
            requirement_position: self.requirements.len(),
            committed: Arc::clone(&self.committed),
            changes: self.changes.share_owner(),
            serializable,
            notification: self.notification.clone(),
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
        for savepoint in self.savepoints[position..].iter().rev() {
            savepoint.changes.release_savepoint(savepoint.id)?;
        }
        truncate_retained(&mut self.savepoints, position);
        Ok(())
    }

    pub(super) fn rollback_to(
        &mut self,
        name: &str,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let position = self.savepoint_position(name)?;
        if let Some(context) = self.serializable.clone() {
            let mark =
                self.savepoints[position]
                    .serializable
                    .ok_or(VersionError::InvalidEncoding(
                        "serializable savepoint has no write mark",
                    ))?;
            context.with_graph(control, |graph| {
                graph.rollback_writes(mark)?;
                self.restore_savepoint(position)
            })
        } else {
            self.restore_savepoint(position)
        }
    }

    fn restore_savepoint(&mut self, position: usize) -> VersionResult<()> {
        let savepoint = &self.savepoints[position];
        savepoint.changes.rollback_to_savepoint(savepoint.id)?;
        self.changes = savepoint.changes.share_owner();
        self.committed = Arc::clone(&savepoint.committed);
        self.notification.clone_from(&savepoint.notification);
        truncate_retained(&mut self.graph, self.savepoints[position].graph_position);
        truncate_retained(&mut self.vector, self.savepoints[position].vector_position);
        truncate_retained(
            &mut self.requirements,
            self.savepoints[position].requirement_position,
        );
        self.savepoints.truncate(position + 1);
        Ok(())
    }

    fn commit_records(
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
        let Some(_) = self.seal_publication(persistence, control)? else {
            return Ok(());
        };
        loop {
            self.prepare_publication_effects(persistence, control)?;
            if self.publish_records(persistence, control)? {
                return Ok(());
            }
        }
    }

    /// Freeze the evaluated batch once. A previously issued mutation origin must resolve its allocation even if every record was undone.
    fn seal_publication(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> VersionResult<Option<StorageTransactionId>> {
        if self.prepared.is_none() {
            self.prepared = Some(self.prepare(control)?);
        }
        let prepared = self.prepared.as_ref().expect("prepared once");
        if prepared.records().is_empty()
            && prepared.graph.is_none()
            && prepared.vector.is_none()
            && prepared.notification.is_none()
            && !prepared.has_requirements()
            && self.allocation.is_none()
        {
            return Ok(None);
        }
        self.ensure_allocation(persistence, control)?;
        Ok(self.allocation)
    }

    fn prepare_publication_effects(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> Result<(), CommitFailure> {
        self.prepare_effects(persistence, control).map_err(|error| {
            self.retain_uncertain_outcome(
                self.allocation.expect("sealed publication"),
                error.into(),
            )
        })
    }

    /// One physical attempt. False permits only re-preparation of derived effects after an authoritative snapshot rejection; no evaluated user changes are replayed.
    fn publish_records(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> Result<bool, CommitFailure> {
        let allocation = self.allocation.expect("sealed publication");
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
                Ok(true)
            }
            Ok(_) => {
                self.outcome = Some(CommitErrorOutcome::Indeterminate(allocation));
                Err(self.retain_uncertain_outcome(allocation, VersionError::CommitMismatch.into()))
            }
            Err(CommitFailure::Rejected(VersionError::CommitSnapshotChanged {
                expected,
                actual,
            })) if prepared.resolved_at == Some(expected) && expected != actual => {
                self.materialized = None;
                Ok(false)
            }
            Err(CommitFailure::Rejected(VersionError::TransactionFinished)) => {
                self.outcome = Some(CommitErrorOutcome::Aborted(allocation));
                Err(VersionError::AlreadyAborted(allocation).into())
            }
            Err(error) => Err(self.retain_uncertain_outcome(allocation, error)),
        }
    }

    fn prepare(&self, control: &StorageReadControl) -> VersionResult<PreparedRecordCommit> {
        self.changes
            .prepare(control)?
            .with_requirements(&self.requirements, control)?
            .with_graph_effects(self.committed.sequence(), &self.graph, control)?
            .with_vector_effects(self.committed.sequence(), &self.vector, control)
            .map(|prepared| prepared.with_notification_effect(self.notification.as_ref()))
    }

    fn prepare_effects(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        use crate::mvcc::resolution::{self, ResolutionMode};
        control.cancellation().check()?;
        let prepared = self.prepared.as_ref().expect("prepared once");
        if resolution::has_effects(prepared) && self.materialized.is_none() {
            self.materialized = resolution::resolve(
                prepared,
                &*self.committed,
                &persistence.snapshot(control)?,
                persistence,
                ResolutionMode::Publication,
                control,
            )?;
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

    fn abort_records(
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
                if receipt.transaction != id
                    || self
                        .prepared
                        .as_ref()
                        .is_none_or(|prepared| receipt.fingerprint != prepared.fingerprint())
                {
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

/// Release an empty mutation or savepoint buffer without allocating during undo. Nonempty buffers retain their original allowance and surviving evaluated inputs.
fn truncate_retained<T>(values: &mut BudgetedVec<T>, len: usize) {
    if len == 0 {
        *values = BudgetedVec::new(values.budget());
    } else {
        values.truncate(len);
    }
}
