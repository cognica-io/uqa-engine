//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::mvcc::commit::RecordWriteKind;
use crate::mvcc::graph::OwnedGraphMutation;
use crate::mvcc::key::RecordKey;
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
}

pub(super) struct Transaction {
    committed: Arc<dyn CommittedRecordSnapshot>,
    pub(super) changes: PrivateRecordChanges,
    read_only: bool,
    pub(super) allocation: Option<StorageTransactionId>,
    prepared: Option<PreparedRecordCommit>,
    materialized: Option<PreparedRecordCommit>,
    graph: BudgetedVec<OwnedGraphMutation>,
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
        if value.is_none() && !exists && kind != RecordWriteKind::GraphCache {
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

    pub(super) fn has_graph_changes(&self) -> bool {
        !self.graph.is_empty()
    }

    pub(super) fn delete_prefix(
        &mut self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<usize> {
        self.writable()?;
        let mut keys = BudgetedVec::new(control.memory());
        self.view()?
            .visit_keys(prefix, None, usize::MAX, control, &mut |key, record| {
                if record.live {
                    keys.push((RecordKey::new(key, control.memory())?, record.revision))?;
                }
                Ok(true)
            })?;
        for (key, expected) in keys.iter() {
            self.changes.apply(
                &[RecordWrite {
                    key: key.bytes(),
                    expected: *expected,
                    value: None,
                }],
                control,
            )?;
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
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self)));
        if !matches!(&result, Ok(Ok(_))) {
            self.changes.rollback_to_savepoint(id)?;
            self.graph.truncate(graph_position);
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
            self.prepared = Some(self.changes.prepare(control)?.with_graph_effects(
                self.committed.sequence(),
                &self.graph,
                control,
            )?);
        }
        let prepared = self.prepared.as_ref().expect("prepared once");
        if prepared.records().is_empty() && prepared.graph.is_none() {
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
        if prepared.graph.is_some() && self.materialized.is_none() {
            let layout = persistence
                .graph_record_layout()
                .ok_or(VersionError::InvalidEncoding(
                    "provider has no graph record layout",
                ))?;
            self.materialized = Some(crate::mvcc::graph::resolve(
                prepared,
                persistence.snapshot(control)?,
                layout,
                persistence.database_id(),
                control,
            )?);
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
