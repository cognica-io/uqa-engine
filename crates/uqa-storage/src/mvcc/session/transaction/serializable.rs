//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session completion orders retained SSI preparation, physical publication and exact outcome resolution.

use std::sync::Arc;

use super::Transaction;
use crate::mvcc::{
    CommitErrorOutcome, CommitStatus, VersionError, VersionResult, VersionedPersistence,
};
use crate::mvcc::{
    SerializableReadContext, SerializableStatus, SerializableWriteMark, TransactionCompletionError,
    TransactionOutcome, TransactionOutcomeId,
};
use crate::read_control::StorageReadControl;
use crate::StorageBackendError;
use crate::StorageBackendResult;

impl Transaction {
    pub(in crate::mvcc::session) fn pending_completion(&self) -> Option<TransactionOutcomeId> {
        self.allocation
            .map(TransactionOutcomeId::Records)
            .or_else(|| self.completion.map(TransactionOutcome::id))
    }

    pub(in crate::mvcc::session) fn serializable_context(
        &self,
    ) -> Option<&SerializableReadContext> {
        self.serializable.as_ref()
    }

    pub(in crate::mvcc::session) fn establish_serializable(
        &mut self,
        persistence: Arc<dyn VersionedPersistence>,
        control: &StorageReadControl,
    ) -> VersionResult<SerializableReadContext> {
        self.unsealed()?;
        if let Some(context) = &self.serializable {
            context.with_graph(control, |graph| graph.check_active(context.id()))?;
            return Ok(context.clone());
        }
        if self.changes.has_written() || self.has_derived_changes() {
            return Err(VersionError::InvalidEncoding(
                "serializable snapshot must precede private writes",
            ));
        }
        let coordinator =
            persistence
                .serializable_coordinator()
                .ok_or(VersionError::InvalidEncoding(
                    "persistence has no serializable coordinator",
                ))?;
        let (participant, committed) =
            coordinator.admit_serializable_snapshot(self.read_only, control)?;
        let context = SerializableReadContext {
            persistence,
            participant,
        };
        let mark = context.with_graph(control, |graph| graph.write_mark(context.id()))?;
        // The first fixed snapshot belongs to the outer transaction, including when acquired after a SQL savepoint.
        for savepoint in &mut *self.savepoints {
            savepoint.serializable = Some(mark);
            savepoint.committed = Arc::clone(&committed);
        }
        self.committed = committed;
        self.serializable = Some(context.clone());
        Ok(context)
    }

    pub(super) fn serializable_mark(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<SerializableWriteMark>> {
        self.serializable
            .as_ref()
            .map(|context| context.with_graph(control, |graph| graph.write_mark(context.id())))
            .transpose()
    }

    pub(in crate::mvcc::session) fn commit(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let Some(context) = self.serializable.clone() else {
            return self
                .commit_records(persistence, control)
                .map_err(super::super::commit_error);
        };
        let result = self.commit_serializable(&context, persistence, control);
        result.map_err(|error| self.completion_error(error))
    }

    fn commit_serializable(
        &mut self,
        context: &SerializableReadContext,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let Some(allocation) = self
            .seal_publication(persistence, control)
            .map_err(VersionError::into_storage_error)?
        else {
            return self.finish_logical(context, true, control);
        };
        let fingerprint = self.prepared.as_ref().expect("prepared once").fingerprint();
        // This admission must finish durably before any main-record commit is attempted.
        context
            .with_graph(control, |graph| match graph.status(context.id())? {
                SerializableStatus::Committed | SerializableStatus::Aborted => {
                    self.retain_graph_outcome(context, graph)?;
                    Ok(())
                }
                _ => graph
                    .prepare_publication(context.id(), allocation, fingerprint, control)
                    .map(|_| ()),
            })
            .map_err(VersionError::into_storage_error)?;
        loop {
            if let Some(result) = self.completed_result(context, true) {
                return result;
            }
            self.prepare_publication_effects(persistence, control)
                .map_err(super::super::commit_error)?;
            let mut attempt = None;
            context
                .with_graph(control, |graph| {
                    self.retain_graph_outcome(context, graph)?;
                    if self.completed() {
                        return Ok(());
                    }
                    let publication = graph
                        .publication(context.id())?
                        .ok_or(VersionError::CommitMismatch)?;
                    if publication.transaction() != allocation
                        || publication.fingerprint() != fingerprint
                    {
                        return Err(VersionError::CommitMismatch);
                    }
                    // Preparation excludes later victim selection; the exact persisted binding remains sealed across retries.
                    if graph.status(context.id())? != SerializableStatus::Prepared {
                        return Err(VersionError::TransactionSealed);
                    }
                    let result = self.publish_records(persistence, control);
                    if let Some(outcome) = self.outcome {
                        self.completion = Some(match outcome {
                            CommitErrorOutcome::Committed(receipt) => {
                                graph.resolve_publication(
                                    publication,
                                    CommitStatus::Committed(receipt),
                                )?;
                                TransactionOutcome::Committed(Self::logical_id(context))
                            }
                            CommitErrorOutcome::Aborted(_) => {
                                graph.resolve_publication(publication, CommitStatus::Aborted)?;
                                TransactionOutcome::Aborted(Self::logical_id(context))
                            }
                            CommitErrorOutcome::Indeterminate(_) => {
                                TransactionOutcome::Indeterminate(Self::logical_id(context))
                            }
                        });
                    }
                    attempt = Some(result);
                    Ok(())
                })
                .map_err(VersionError::into_storage_error)?;
            if let Some(result) = self.completed_result(context, true) {
                return result;
            }
            match attempt.ok_or_else(|| {
                VersionError::InvalidEncoding("serializable publication was not attempted")
                    .into_storage_error()
            })? {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => return Err(super::super::commit_error(error)),
            }
        }
    }

    fn retain_graph_outcome(
        &mut self,
        context: &SerializableReadContext,
        graph: &mut crate::mvcc::SerializableGraph,
    ) -> VersionResult<()> {
        let status = graph.status(context.id())?;
        if !matches!(
            status,
            SerializableStatus::Committed | SerializableStatus::Aborted
        ) {
            return Ok(());
        }
        if let Some(publication) = graph.publication(context.id())? {
            if self.allocation != Some(publication.transaction())
                || self
                    .prepared
                    .as_ref()
                    .is_none_or(|prepared| prepared.fingerprint() != publication.fingerprint())
            {
                return Err(VersionError::CommitMismatch);
            }
            self.outcome = Some(
                match graph.resolve_publication(publication, CommitStatus::Unknown)? {
                    CommitStatus::Committed(receipt) => CommitErrorOutcome::Committed(receipt),
                    CommitStatus::Aborted => CommitErrorOutcome::Aborted(publication.transaction()),
                    _ => return Err(VersionError::CommitMismatch),
                },
            );
        } else if status == SerializableStatus::Committed && self.allocation.is_some() {
            return Err(VersionError::CommitMismatch);
        }
        let id = Self::logical_id(context);
        self.completion = Some(if status == SerializableStatus::Committed {
            TransactionOutcome::Committed(id)
        } else {
            TransactionOutcome::Aborted(id)
        });
        Ok(())
    }

    fn finish_logical(
        &mut self,
        context: &SerializableReadContext,
        commit: bool,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        context
            .with_graph(control, |graph| {
                self.retain_graph_outcome(context, graph)?;
                if self.completed() {
                    return Ok(());
                }
                if graph.publication(context.id())?.is_some() {
                    return Err(VersionError::CommitMismatch);
                }
                if commit {
                    graph.prepare_commit(context.id(), control)?;
                }
                self.completion =
                    Some(TransactionOutcome::Indeterminate(Self::logical_id(context)));
                if commit {
                    graph.commit(context.id())
                } else {
                    graph.rollback(context.id())
                }
            })
            .map_err(VersionError::into_storage_error)?;
        if let Some(result) = self.completed_result(context, commit) {
            return result;
        }
        self.completion = Some(if commit {
            TransactionOutcome::Committed(Self::logical_id(context))
        } else {
            TransactionOutcome::Aborted(Self::logical_id(context))
        });
        Ok(())
    }

    pub(in crate::mvcc::session) fn abort(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let Some(context) = self.serializable.clone() else {
            return self
                .abort_records(persistence, control)
                .map_err(super::super::commit_error);
        };
        let result = if self.allocation.is_none() {
            self.finish_logical(&context, false, control)
        } else {
            context
                .with_graph(control, |graph| {
                    self.retain_graph_outcome(&context, graph)?;
                    if self.completed() {
                        return Ok(());
                    }
                    let result = self.abort_records(persistence, control);
                    if let Some(publication) = graph.publication(context.id())? {
                        let status = match self.outcome {
                            Some(CommitErrorOutcome::Committed(receipt)) => {
                                CommitStatus::Committed(receipt)
                            }
                            Some(CommitErrorOutcome::Aborted(_)) => CommitStatus::Aborted,
                            _ => CommitStatus::Pending,
                        };
                        graph.resolve_publication(publication, status)?;
                    } else if matches!(self.outcome, Some(CommitErrorOutcome::Aborted(_))) {
                        graph.rollback(context.id())?;
                    }
                    match self.outcome {
                        Some(CommitErrorOutcome::Committed(_)) => {
                            self.completion =
                                Some(TransactionOutcome::Committed(Self::logical_id(&context)));
                        }
                        Some(
                            CommitErrorOutcome::Aborted(_) | CommitErrorOutcome::Indeterminate(_),
                        ) => {
                            self.completion = Some(TransactionOutcome::Indeterminate(
                                Self::logical_id(&context),
                            ));
                        }
                        None => {}
                    }
                    result.map_err(|error| super::super::commit_error(error).into())
                })
                .map_err(VersionError::into_storage_error)
                .and_then(|()| {
                    self.completed_result(&context, false).unwrap_or_else(|| {
                        self.completion =
                            Some(TransactionOutcome::Aborted(Self::logical_id(&context)));
                        Ok(())
                    })
                })
        };
        result.map_err(|error| self.completion_error(error))
    }

    fn logical_id(context: &SerializableReadContext) -> TransactionOutcomeId {
        TransactionOutcomeId::Serializable(context.id())
    }

    fn completed(&self) -> bool {
        matches!(
            self.completion,
            Some(TransactionOutcome::Committed(_) | TransactionOutcome::Aborted(_))
        )
    }

    fn completed_result(
        &self,
        context: &SerializableReadContext,
        committing: bool,
    ) -> Option<StorageBackendResult<()>> {
        match (self.completion, committing) {
            (Some(TransactionOutcome::Committed(_)), true)
            | (Some(TransactionOutcome::Aborted(_)), false) => Some(Ok(())),
            (Some(TransactionOutcome::Committed(_) | TransactionOutcome::Aborted(_)), _) => {
                Some(Err(StorageBackendError::Other(format!(
                    "transaction {:?} already completed with the opposite outcome",
                    context.id()
                ))))
            }
            _ => None,
        }
    }

    fn completion_error(&self, source: StorageBackendError) -> StorageBackendError {
        if let Some(outcome) = self
            .outcome
            .map(TransactionOutcome::from)
            .or(self.completion)
        {
            StorageBackendError::backend(
                "MVCC",
                TransactionCompletionError {
                    outcome,
                    publication: self.outcome,
                    source,
                },
            )
        } else {
            source
        }
    }
}
