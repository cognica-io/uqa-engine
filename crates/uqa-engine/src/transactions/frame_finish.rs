//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! COMMIT, ROLLBACK, and retained nontransactional sequence effects.

use super::{
    Engine, NontransactionalSequenceValues, SQLError, SessionLastSequenceReference,
    SessionStateSnapshot, StorageBackendError, StorageBackendResult, StorageSavepointId,
    TransactionDirtyState, TransactionFrame, TransactionIntent, TransactionStatus,
};
use crate::notifications::NotificationCommitGuard;
use uqa_execution::row_locks::{temporary_roles::TemporaryRolePublication, RowChangePublication};
use uqa_storage::mvcc::TransactionOutcome;

// Drop temporary additions before the notification and row-publication guards are released.
struct TransactionPublication<'a> {
    temporary_roles: Option<TemporaryRolePublication<'a>>,
    notifications: Option<NotificationCommitGuard<'a>>,
    changes: Option<RowChangePublication<'a>>,
}

impl Engine {
    pub(super) fn commit_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
        deferred_constraints_validated: bool,
    ) -> Result<(), SQLError> {
        let storage_savepoint = stack
            .last()
            .ok_or_else(|| SQLError::Internal("COMMIT without an open transaction".into()))?
            .storage_savepoint;
        let resolving = matches!(
            stack.last().map(|frame| frame.status),
            Some(TransactionStatus::CommitPending(_))
        );
        if !deferred_constraints_validated && storage_savepoint.is_none() && !resolving {
            return Err(SQLError::Internal(
                "outer COMMIT skipped deferred-constraint preparation".into(),
            ));
        }
        let frame = stack
            .last()
            .ok_or_else(|| SQLError::Internal("COMMIT without an open transaction".into()))?;
        let read_only = frame.intent == TransactionIntent::ReadOnly;
        let statistics_changes =
            (storage_savepoint.is_none() && !resolving).then(|| frame.statistics_changes.clone());
        if !resolving {
            self.validate_read_only_commit(stack, read_only, storage_savepoint.is_none())?;
        }
        let mut publication =
            self.prepare_transaction_publication(stack, storage_savepoint.is_none())?;
        let savepoints_deferred = Self::backend_savepoints_deferred(stack);
        if let Some(statistics_changes) = statistics_changes {
            // Maintenance counters are derived at publication, after any earlier publisher. A savepoint can restore an older command base, and concurrent commands must not overwrite each other's accumulated maintenance state.
            let refresh = if statistics_changes.is_empty() {
                Ok(())
            } else {
                self.storage
                    .backend
                    .as_ref()
                    .filter(|backend| backend.transaction_model().is_versioned())
                    .map_or(Ok(()), |backend| {
                        backend.refresh_transaction_snapshot(&self.runtime.cancellation)
                    })
            };
            if let Err(error) =
                refresh.and_then(|()| self.persist_statistics_changes(&statistics_changes))
            {
                drop(publication);
                return Err(self.rollback_failed_statistics_preparation(stack, &error));
            }
        }
        if let Some(backend) = self.storage.backend.as_ref() {
            let commit_result = if let Some(savepoint) = storage_savepoint {
                if savepoints_deferred {
                    Ok(())
                } else {
                    backend.release_savepoint(savepoint)
                }
            } else {
                backend.commit_transaction()
            };
            if let Err(error) = commit_result {
                drop(publication);
                if let Some(error) = Self::retain_pending_commit(stack, &error) {
                    return Err(error);
                }
                let action = if storage_savepoint.is_some() {
                    "nested COMMIT savepoint"
                } else {
                    "COMMIT"
                };
                return Err(self.recover_failed_transaction_finish(
                    stack,
                    storage_savepoint.is_some(),
                    Self::storage_tx_error(action, &error),
                ));
            }
        }
        if let Some(temporary_roles) = publication.temporary_roles.take() {
            temporary_roles.commit();
        }
        let committed = stack
            .pop()
            .ok_or_else(|| SQLError::Internal("COMMIT lost its transaction frame".into()))?;
        self.publish_committed_transaction_frame(
            stack,
            committed,
            publication.changes,
            publication.notifications,
        )
    }

    fn prepare_transaction_publication<'a>(
        &'a self,
        stack: &mut Vec<TransactionFrame>,
        outer: bool,
    ) -> Result<TransactionPublication<'a>, SQLError> {
        let frame = stack
            .last()
            .ok_or_else(|| SQLError::Internal("COMMIT without an open transaction".into()))?;
        let read_only = frame.intent == TransactionIntent::ReadOnly;
        let status = frame.status;
        let has_row_changes = !frame.row_changes.is_empty();
        let change_publication = if outer
            && (has_row_changes || (self.versioned_backend_transactions() && !read_only))
        {
            Some(
                self.row_locks
                    .begin_change_publication(&self.runtime.cancellation)
                    .map_err(|error| match status {
                        TransactionStatus::CommitPending(transaction) => {
                            Self::pending_commit_error(transaction, error)
                        }
                        _ => error,
                    })?,
            )
        } else {
            None
        };
        let notification_commit = self.prepare_notification_commit(stack, outer)?;
        let temporary_roles = if outer {
            match self.prepare_temporary_role_publication() {
                Ok(publication) => publication,
                Err(error) => {
                    drop(notification_commit);
                    drop(change_publication);
                    if let TransactionStatus::CommitPending(transaction) = status {
                        return Err(Self::pending_commit_error(transaction, error));
                    }
                    return Err(match self.rollback_transaction_frame(stack) {
                        Ok(()) => error,
                        Err(rollback) => SQLError::Internal(format!("{error}; temporary catalog preparation rollback also failed: {rollback}")),
                    });
                }
            }
        } else {
            None
        };
        Ok(TransactionPublication {
            temporary_roles,
            notifications: notification_commit,
            changes: change_publication,
        })
    }

    fn rollback_failed_statistics_preparation(
        &self,
        stack: &mut Vec<TransactionFrame>,
        error: &StorageBackendError,
    ) -> SQLError {
        let failure = Self::storage_tx_error("statistics maintenance state", error);
        // This error precedes COMMIT; the live/poisoned backend still needs
        // rollback before its transaction frame and caches can be restored.
        match self.rollback_transaction_frame(stack) {
            Ok(()) => failure,
            Err(rollback) => SQLError::Internal(format!(
                "{failure}; statistics preparation rollback also failed: {rollback}"
            )),
        }
    }

    fn validate_read_only_commit(
        &self,
        stack: &mut Vec<TransactionFrame>,
        read_only: bool,
        outer: bool,
    ) -> Result<(), SQLError> {
        if !read_only || !outer {
            return Ok(());
        }
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(());
        };
        let violation = match backend.transaction_has_written() {
            Ok(false) => return Ok(()),
            Ok(true) => SQLError::Internal(
                "read-only SQL execution attempted to mutate persistent storage".into(),
            ),
            Err(error) => SQLError::Internal(format!(
                "inspect read-only transaction before COMMIT: {error}"
            )),
        };
        Err(match self.rollback_transaction_frame(stack) {
            Ok(()) => violation,
            Err(rollback_error) => SQLError::Internal(format!(
                "{violation}; read-only violation rollback also failed: {rollback_error}"
            )),
        })
    }

    fn prepare_notification_commit<'a>(
        &'a self,
        stack: &mut Vec<TransactionFrame>,
        outer: bool,
    ) -> Result<Option<NotificationCommitGuard<'a>>, SQLError> {
        let notification_commit = {
            let frame = stack
                .last()
                .ok_or_else(|| SQLError::Internal("COMMIT without an open transaction".into()))?;
            self.begin_notification_commit(outer, frame)
        };
        match notification_commit {
            Ok(commit) => Ok(commit),
            Err(error) => {
                if let Some(TransactionStatus::CommitPending(transaction)) =
                    stack.last().map(|frame| frame.status)
                {
                    return Err(Self::pending_commit_error(transaction, error));
                }
                Err(match self.rollback_transaction_frame(stack) {
                    Ok(()) => error,
                    Err(rollback_error) => SQLError::Internal(format!(
                        "{error}; notification commit preparation rollback also failed: {rollback_error}"
                    )),
                })
            }
        }
    }

    /// End any retained backend transaction before restoring caches. If cleanup fails while storage still retains its transaction, keep the engine frames and locks for a later explicit resolution instead of reading private state as committed state.
    pub(super) fn recover_failed_transaction_finish(
        &self,
        stack: &mut Vec<TransactionFrame>,
        nested: bool,
        finish_error: SQLError,
    ) -> SQLError {
        let nontransactional_sequence_values = stack
            .first()
            .map(|frame| frame.nontransactional_sequence_values.clone())
            .unwrap_or_default();
        let session_snapshot = stack.first().map(|frame| frame.session_snapshot.clone());
        let snapshot = stack.first().and_then(|frame| frame.data_snapshot.clone());
        let dirty_at_begin = stack
            .first()
            .map_or_else(TransactionDirtyState::default, |frame| frame.dirty_at_begin);
        let mut cleanup_errors =
            match self.abort_retained_backend_before_restore(stack, nested, &finish_error) {
                Ok(error) => error
                    .into_iter()
                    .map(|error| format!("storage rollback: {error}"))
                    .collect::<Vec<_>>(),
                Err(error) => return error,
            };
        let outer_notification_transaction = !stack.is_empty();
        stack.clear();
        self.row_locks.release_session(self.session_id);
        if let Some(snapshot) = session_snapshot.as_ref() {
            self.restore_graph_transaction_overlay(snapshot);
        }
        self.restore_transaction_dirty_state(dirty_at_begin);
        if let Some(snapshot) = snapshot.as_ref() {
            if let Err(error) = self.restore_transaction_data(snapshot) {
                cleanup_errors.push(format!("memory restore: {error}"));
            }
        }
        if self.storage.backend.is_some() {
            if let Err(error) = self.reload_persistent_value_indexes() {
                cleanup_errors.push(format!("btree restore: {error}"));
            }
            if let Err(error) = self.reload_table_catalog_after_rollback() {
                cleanup_errors.push(format!("table catalog restore: {error}"));
            }
            if let Err(error) = self.reload_catalog_registries_after_rollback() {
                cleanup_errors.push(format!("registry restore: {error}"));
            }
        }
        if session_snapshot.is_some() {
            if let Err(error) = self.persist_nontransactional_sequence_values_after_rollback(
                &nontransactional_sequence_values,
                true,
            ) {
                cleanup_errors.push(format!("sequence value restore: {error}"));
            }
        }
        if let Some(snapshot) = session_snapshot.as_ref() {
            self.restore_session_state(snapshot);
        }
        if outer_notification_transaction {
            if let Err(error) = self.rollback_notification_state() {
                cleanup_errors.push(format!("notification state restore: {error}"));
            }
        }
        self.apply_nontransactional_sequence_values(&nontransactional_sequence_values);
        if cleanup_errors.is_empty() {
            finish_error
        } else {
            SQLError::Internal(format!(
                "{finish_error}; failed transaction cleanup: {}",
                cleanup_errors.join("; ")
            ))
        }
    }

    fn abort_retained_backend_before_restore(
        &self,
        stack: &mut [TransactionFrame],
        nested: bool,
        finish_error: &SQLError,
    ) -> Result<Option<StorageBackendError>, SQLError> {
        let mut cleanup_error = None;
        if let Some(backend) = self.storage.backend.as_ref() {
            if nested || backend.in_transaction() {
                if let Err(error) = backend.rollback_transaction() {
                    if let Some(error) = Self::retain_pending_commit(stack, &error) {
                        return Err(error);
                    }
                    if backend.in_transaction() {
                        for frame in stack.iter_mut() {
                            frame.status = TransactionStatus::Failed;
                        }
                        return Err(SQLError::Internal(format!(
                            "{finish_error}; storage rollback failed; transaction state is retained: {error}"
                        )));
                    }
                    cleanup_error = Some(error);
                }
                if backend.in_transaction() {
                    for frame in stack.iter_mut() {
                        frame.status = TransactionStatus::Failed;
                    }
                    return Err(SQLError::Internal(format!(
                        "{finish_error}; storage rollback did not end the transaction; transaction state is retained"
                    )));
                }
            }
        }
        Ok(cleanup_error)
    }

    pub(super) fn rollback_backend_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
        storage_savepoint: Option<StorageSavepointId>,
        backend_aborted: bool,
    ) -> Result<(), SQLError> {
        let savepoints_deferred = Self::backend_savepoints_deferred(stack);
        let Some(backend) = self.storage.backend.as_ref().filter(|_| !backend_aborted) else {
            return Ok(());
        };
        let (action, rollback_result) = if let Some(savepoint) = storage_savepoint {
            if savepoints_deferred {
                ("nested ROLLBACK savepoint", Ok(()))
            } else {
                match backend.rollback_to_savepoint(savepoint) {
                    Ok(()) => (
                        "nested ROLLBACK release",
                        backend.release_savepoint(savepoint),
                    ),
                    Err(error) => ("nested ROLLBACK savepoint", Err(error)),
                }
            }
        } else {
            ("ROLLBACK", backend.rollback_transaction())
        };
        if let Err(error) = rollback_result {
            if storage_savepoint.is_none() {
                if let Some(TransactionOutcome::Committed(transaction)) =
                    error.transaction_outcome()
                {
                    stack
                        .last_mut()
                        .ok_or_else(|| {
                            SQLError::Internal("commit receipt without a transaction frame".into())
                        })?
                        .status = TransactionStatus::CommitPending(transaction);
                    self.commit_transaction_frame(stack, false)?;
                    return Err(SQLError::Routine {
                        sqlstate: "25000".into(),
                        message: format!(
                            "transaction {transaction:?} already committed; ROLLBACK cannot undo it"
                        ),
                    });
                }
            }
            if let Some(error) = Self::retain_pending_commit(stack, &error) {
                return Err(error);
            }
            return Err(self.recover_failed_transaction_finish(
                stack,
                storage_savepoint.is_some(),
                Self::storage_tx_error(action, &error),
            ));
        }
        Ok(())
    }

    fn retain_pending_commit(
        stack: &mut [TransactionFrame],
        error: &StorageBackendError,
    ) -> Option<SQLError> {
        let frame = stack.last_mut()?;
        if frame.storage_savepoint.is_some() {
            return None;
        }
        let transaction = match error.transaction_outcome() {
            Some(
                TransactionOutcome::Indeterminate(transaction)
                | TransactionOutcome::Committed(transaction),
            ) => transaction,
            Some(TransactionOutcome::Aborted(_)) => {
                frame.status = TransactionStatus::Failed;
                return None;
            }
            None => match frame.status {
                TransactionStatus::CommitPending(transaction) => transaction,
                _ => return None,
            },
        };
        frame.status = TransactionStatus::CommitPending(transaction);
        Some(Self::pending_commit_error(transaction, error))
    }

    pub(super) fn rollback_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
    ) -> Result<(), SQLError> {
        let nontransactional_sequence_values = stack
            .last()
            .map(|frame| frame.nontransactional_sequence_values.clone())
            .unwrap_or_default();
        let storage_savepoint = stack
            .last()
            .ok_or_else(|| SQLError::Internal("ROLLBACK without an open transaction".into()))?
            .storage_savepoint;
        let backend_aborted = stack
            .last()
            .is_some_and(|frame| frame.status == TransactionStatus::FailedBackendAborted);
        self.rollback_backend_transaction_frame(stack, storage_savepoint, backend_aborted)?;
        let frame = stack.last().ok_or_else(|| {
            SQLError::Internal("ROLLBACK lost its checked transaction frame".into())
        })?;
        let session_snapshot = frame.session_snapshot.clone();
        self.restore_graph_transaction_overlay(&session_snapshot);
        let mut cleanup_errors = Vec::new();
        if let Some(snapshot) = frame.data_snapshot.as_ref() {
            if let Err(error) = self.restore_transaction_data(snapshot) {
                cleanup_errors.push(format!("memory restore: {error}"));
            }
        }
        let dirty_at_begin = stack
            .last()
            .map_or_else(TransactionDirtyState::default, |frame| frame.dirty_at_begin);
        self.restore_transaction_dirty_state(dirty_at_begin);
        if let Err(error) = self.reload_persistent_value_indexes() {
            cleanup_errors.push(format!("btree restore: {error}"));
        }
        if self.storage.backend.is_some() {
            if let Err(error) = self.reload_table_catalog_after_rollback() {
                cleanup_errors.push(format!("table catalog restore: {error}"));
            }
            if let Err(error) = self.reload_catalog_registries_after_rollback() {
                cleanup_errors.push(format!("registry restore: {error}"));
            }
        }
        if let Err(error) = self.persist_nontransactional_sequence_values_after_rollback(
            &nontransactional_sequence_values,
            storage_savepoint.is_none(),
        ) {
            cleanup_errors.push(format!("sequence value restore: {error}"));
        }
        self.restore_session_state(&session_snapshot);
        self.apply_nontransactional_sequence_values(&nontransactional_sequence_values);
        let begin_lock_mark = frame.begin_lock_mark;
        let first_snapshot_set = frame.first_snapshot_set;
        stack.pop();
        if stack.is_empty() {
            if let Err(error) = self.rollback_notification_state() {
                cleanup_errors.push(format!("notification state restore: {error}"));
            }
            self.row_locks.release_session(self.session_id);
        } else {
            self.row_locks
                .release_mark_above(self.session_id, begin_lock_mark.saturating_sub(1));
            if let Some(parent) = stack.last_mut() {
                parent.first_snapshot_set |= first_snapshot_set;
            }
        }
        if cleanup_errors.is_empty() {
            Ok(())
        } else {
            Err(SQLError::Internal(format!(
                "ROLLBACK completed but engine state restoration failed: {}",
                cleanup_errors.join("; ")
            )))
        }
    }

    pub(super) fn persist_nontransactional_sequence_values_after_rollback(
        &self,
        values: &NontransactionalSequenceValues,
        outer: bool,
    ) -> StorageBackendResult<()> {
        if self.storage.catalog.is_none() {
            return Ok(());
        }
        let persistent = {
            let sequences = self.durable.sequences.read();
            let object_ids = self.durable.sequence_object_ids.read();
            let persistence = self.durable.sequence_persistence.read();
            values
                .iter()
                .filter_map(|(history_object_id, history)| {
                    let (relation, object_id) = object_ids
                        .iter()
                        .find(|(_, object_id)| *object_id == history_object_id)?;
                    if persistence.get(relation).copied().unwrap_or_default()
                        == uqa_sql::ast::RelationPersistence::Temporary
                    {
                        return None;
                    }
                    let generation = sequences.get(relation)?.definition_generation;
                    let value = history.values_by_definition.get(&generation).copied()?;
                    (value.object_id == *object_id && !value.autonomous).then(|| {
                        (
                            relation.qualified_name(),
                            value.object_id,
                            generation,
                            value,
                        )
                    })
                })
                .collect::<Vec<_>>()
        };
        if persistent.is_empty() {
            return Ok(());
        }
        let persist = |catalog: &dyn uqa_storage::CatalogFacade| -> StorageBackendResult<()> {
            for (name, object_id, generation, value) in &persistent {
                match catalog
                    .set_sequence_value(
                        name,
                        *object_id,
                        *generation,
                        value.current,
                        value.called,
                        value.log_count,
                    )? {
                    uqa_storage::SequenceSetValueResult::Set(_) => {}
                    uqa_storage::SequenceSetValueResult::Missing => return Err(StorageBackendError::Other(format!(
                        "sequence `{name}` disappeared while restoring its nontransactional value"
                    ))),
                    uqa_storage::SequenceSetValueResult::DefinitionChanged => return Err(StorageBackendError::Other(format!(
                        "sequence `{name}` definition changed while restoring its nontransactional value"
                    ))),
                }
            }
            Ok(())
        };
        if !outer {
            return persist(self.storage.catalog.as_deref().ok_or_else(|| {
                StorageBackendError::Other(
                    "persistent sequence values require a catalog after rollback".into(),
                )
            })?);
        }
        let provider = self.storage.provider.as_ref().ok_or_else(|| {
            StorageBackendError::Other(
                "nontransactional sequence values require an independent session".into(),
            )
        })?;
        let session = provider.open_session()?;
        session.backend.begin_transaction()?;
        match persist(session.catalog.as_ref()) {
            Ok(()) => session.backend.commit_transaction(),
            Err(error) => match session.backend.rollback_transaction() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(StorageBackendError::Other(format!(
                    "restore nontransactional sequence values failed: {error}; rollback also failed: {rollback_error}"
                ))),
            },
        }
    }

    pub(super) fn apply_nontransactional_sequence_values(
        &self,
        values: &NontransactionalSequenceValues,
    ) {
        let mut sequences = self.durable.sequences.write();
        let object_ids = self.durable.sequence_object_ids.read();
        let mut session = self.session.state.write();
        if let Some((history_object_id, history)) =
            values.iter().find(|(_, history)| history.defines_lastval)
        {
            session.last_sequence = object_ids
                .iter()
                .find(|(_, object_id)| *object_id == history_object_id)
                .map(|(relation, _)| SessionLastSequenceReference {
                    relation: relation.clone(),
                    object_id: history.object_id,
                });
        }
        for (history_object_id, history) in values {
            let Some((relation, object_id)) = object_ids
                .iter()
                .find(|(_, object_id)| *object_id == history_object_id)
            else {
                continue;
            };
            let Some(generation) = sequences
                .get(relation)
                .map(|sequence| sequence.definition_generation)
            else {
                continue;
            };
            if let Some(value) = history
                .values_by_definition
                .get(&generation)
                .filter(|value| value.object_id == *object_id)
            {
                if let Some(sequence) = sequences.get_mut(relation) {
                    sequence.current = value.current;
                    sequence.called = value.called;
                    sequence.log_count = value.log_count;
                }
            }
            if history.object_id == *object_id {
                session
                    .sequence_currvals
                    .retain(|_, current| current.object_id != *object_id);
                if let Some(currval) = history.session_currval {
                    session.sequence_currvals.insert(relation.clone(), currval);
                }
            }
        }
    }

    pub(super) fn restore_session_state_preserving_sequences(
        &self,
        snapshot: &SessionStateSnapshot,
        values: &NontransactionalSequenceValues,
        outer: bool,
        cleanup_errors: &mut Vec<String>,
    ) {
        if let Err(error) =
            self.persist_nontransactional_sequence_values_after_rollback(values, outer)
        {
            cleanup_errors.push(format!("sequence value restore: {error}"));
        }
        self.restore_session_state(snapshot);
        self.apply_nontransactional_sequence_values(values);
    }
}
