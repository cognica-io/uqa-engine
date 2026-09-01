//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! COMMIT, ROLLBACK, and nontransactional statistics restoration.

use super::{
    Engine, NontransactionalColumnStats, NontransactionalSequenceValues, SQLError,
    SessionStateSnapshot, StorageBackendError, StorageBackendResult, StorageSavepointId,
    TransactionDirtyState, TransactionFrame, TransactionIntent, TransactionRelationStates,
    TransactionStatus,
};

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
        if !deferred_constraints_validated && storage_savepoint.is_none() {
            return Err(SQLError::Internal(
                "outer COMMIT skipped deferred-constraint preparation".into(),
            ));
        }
        let frame = stack
            .last()
            .ok_or_else(|| SQLError::Internal("COMMIT without an open transaction".into()))?;
        let read_only = frame.intent == TransactionIntent::ReadOnly;
        if read_only && storage_savepoint.is_none() {
            if let Some(backend) = self.storage.backend.as_ref() {
                let violation = match backend.transaction_has_written() {
                    Ok(false) => None,
                    Ok(true) => Some(SQLError::Internal(
                        "read-only SQL execution attempted to mutate persistent storage".into(),
                    )),
                    Err(error) => Some(SQLError::Internal(format!(
                        "inspect read-only transaction before COMMIT: {error}"
                    ))),
                };
                if let Some(violation) = violation {
                    return Err(match self.rollback_transaction_frame(stack) {
                        Ok(()) => violation,
                        Err(rollback_error) => SQLError::Internal(format!(
                            "{violation}; read-only violation rollback also failed: {rollback_error}"
                        )),
                    });
                }
            }
        }
        let change_publication = if storage_savepoint.is_none() && !frame.row_changes.is_empty() {
            Some(
                self.row_locks
                    .begin_change_publication(&self.runtime.cancellation)?,
            )
        } else {
            None
        };
        let savepoints_deferred = Self::backend_savepoints_deferred(stack);
        if let Some(backend) = self.storage.backend.as_ref() {
            let commit_result = if let Some(savepoint) = storage_savepoint {
                if savepoints_deferred {
                    Ok(())
                } else {
                    backend
                        .release_savepoint(savepoint)
                        .map_err(|err| Self::storage_tx_error("nested COMMIT savepoint", &err))
                }
            } else {
                backend
                    .commit_transaction()
                    .map_err(|err| Self::storage_tx_error("COMMIT", &err))
            };
            if let Err(commit_error) = commit_result {
                return Err(self.recover_failed_transaction_finish(
                    stack,
                    storage_savepoint.is_some(),
                    commit_error,
                ));
            }
        }
        let committed = stack
            .pop()
            .ok_or_else(|| SQLError::Internal("COMMIT lost its transaction frame".into()))?;
        if storage_savepoint.is_none() {
            let publication_result = self.row_locks.publish_row_changes(
                self.session_id,
                committed.row_changes.iter().map(|change| change.pending),
            );
            drop(change_publication);
            self.row_locks.release_session(self.session_id);
            self.publish_committed_transaction_epochs();
            publication_result?;
            self.session
                .portals
                .lock()
                .retain(|_, portal| portal.holdable);
        }
        if let Some(parent) = stack.last_mut() {
            parent.next_lock_mark = parent.next_lock_mark.max(committed.next_lock_mark);
            parent.constraint_modes = committed.constraint_modes;
            parent.row_changes.extend(committed.row_changes);
            parent.deferred_foreign_key_checks = committed.deferred_foreign_key_checks;
            parent.deferred_constraint_trigger_events =
                committed.deferred_constraint_trigger_events;
            parent.first_snapshot_set |= committed.first_snapshot_set;
        }
        Ok(())
    }

    /// A failed outer backend COMMIT/ROLLBACK has already ended the managed
    /// storage transaction; a failed nested savepoint finish aborts the
    /// enclosing transaction explicitly. In every case the engine stack and
    /// session-local caches are restored before the error escapes, so callers
    /// never inherit a ghost transaction or uncommitted catalog state.
    pub(super) fn recover_failed_transaction_finish(
        &self,
        stack: &mut Vec<TransactionFrame>,
        nested: bool,
        finish_error: SQLError,
    ) -> SQLError {
        let raw_nontransactional_column_stats = stack
            .first()
            .map(|frame| frame.nontransactional_column_stats.clone())
            .unwrap_or_default();
        let nontransactional_sequence_values = stack
            .first()
            .map(|frame| frame.nontransactional_sequence_values.clone())
            .unwrap_or_default();
        let rollback_relation_states = stack
            .first()
            .map(|frame| frame.relation_states_at_begin.clone())
            .unwrap_or_default();
        let nontransactional_column_stats = self.nontransactional_column_stats_after_rollback(
            &raw_nontransactional_column_stats,
            &rollback_relation_states,
        );
        let session_snapshot = stack.first().map(|frame| frame.session_snapshot.clone());
        let snapshot = stack.first().and_then(|frame| frame.data_snapshot.clone());
        let dirty_at_begin = stack
            .first()
            .map_or_else(TransactionDirtyState::default, |frame| frame.dirty_at_begin);
        let mut cleanup_errors = Vec::new();
        if nested {
            if let Some(backend) = self.storage.backend.as_ref() {
                if let Err(error) = backend.rollback_transaction() {
                    cleanup_errors.push(format!("storage rollback: {error}"));
                }
            }
        }
        stack.clear();
        self.row_locks.release_session(self.session_id);
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
        if let Err(error) = self.persist_nontransactional_column_stats_after_rollback(
            &nontransactional_column_stats,
            true,
        ) {
            cleanup_errors.push(format!("ANALYZE statistics restore: {error}"));
        }
        if let Err(error) = self.apply_nontransactional_column_stats(&nontransactional_column_stats)
        {
            cleanup_errors.push(format!("ANALYZE statistics cache restore: {error}"));
        }
        if let Err(error) = self.persist_nontransactional_sequence_values_after_rollback(
            &nontransactional_sequence_values,
            true,
        ) {
            cleanup_errors.push(format!("sequence value restore: {error}"));
        }
        if let Some(snapshot) = session_snapshot.as_ref() {
            self.restore_session_state(snapshot);
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

    pub(super) fn retain_nontransactional_stats_for_rollback(
        &self,
        stack: &mut [TransactionFrame],
        relation_states: &TransactionRelationStates,
    ) -> NontransactionalColumnStats {
        let raw_nontransactional_column_stats = stack
            .first()
            .map(|frame| frame.nontransactional_column_stats.clone())
            .unwrap_or_default();
        let nontransactional_column_stats = self.nontransactional_column_stats_after_rollback(
            &raw_nontransactional_column_stats,
            relation_states,
        );
        if let Some(frame) = stack.first_mut() {
            frame
                .nontransactional_column_stats
                .clone_from(&nontransactional_column_stats);
        }
        nontransactional_column_stats
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
        let rollback_result = if let Some(savepoint) = storage_savepoint {
            if savepoints_deferred {
                Ok(())
            } else {
                backend
                    .rollback_to_savepoint(savepoint)
                    .map_err(|error| Self::storage_tx_error("nested ROLLBACK savepoint", &error))
                    .and_then(|()| {
                        backend.release_savepoint(savepoint).map_err(|error| {
                            Self::storage_tx_error("nested ROLLBACK release", &error)
                        })
                    })
            }
        } else {
            backend
                .rollback_transaction()
                .map_err(|error| Self::storage_tx_error("ROLLBACK", &error))
        };
        rollback_result.map_err(|rollback_error| {
            self.recover_failed_transaction_finish(
                stack,
                storage_savepoint.is_some(),
                rollback_error,
            )
        })
    }

    pub(super) fn rollback_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
    ) -> Result<(), SQLError> {
        let nontransactional_sequence_values = stack
            .last()
            .map(|frame| frame.nontransactional_sequence_values.clone())
            .unwrap_or_default();
        let rollback_relation_states = stack
            .last()
            .map(|frame| frame.relation_states_at_begin.clone())
            .unwrap_or_default();
        let nontransactional_column_stats =
            self.retain_nontransactional_stats_for_rollback(stack, &rollback_relation_states);
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
        if let Err(error) = self.persist_nontransactional_column_stats_after_rollback(
            &nontransactional_column_stats,
            storage_savepoint.is_none(),
        ) {
            cleanup_errors.push(format!("ANALYZE statistics restore: {error}"));
        }
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
        if let Err(error) = self.apply_nontransactional_column_stats(&nontransactional_column_stats)
        {
            cleanup_errors.push(format!("ANALYZE statistics cache restore: {error}"));
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

    pub(super) fn persist_nontransactional_column_stats_after_rollback(
        &self,
        stats: &NontransactionalColumnStats,
        outer: bool,
    ) -> StorageBackendResult<()> {
        if !stats
            .iter()
            .any(|entry| entry.persistent && !entry.autonomous)
        {
            return Ok(());
        }
        if !outer {
            let catalog = self.storage.catalog.as_ref().ok_or_else(|| {
                StorageBackendError::Other("persistent ANALYZE statistics require a catalog".into())
            })?;
            for entry in stats
                .iter()
                .filter(|entry| entry.persistent && !entry.autonomous)
            {
                Self::persist_column_stats(catalog.as_ref(), &entry.table_name, &entry.stats)?;
            }
            return Ok(());
        }
        let provider = self.storage.provider.as_ref().ok_or_else(|| {
            StorageBackendError::Other(
                "nontransactional ANALYZE statistics require an independent session".into(),
            )
        })?;
        let session = provider.open_session()?;
        session.backend.begin_transaction()?;
        let result = (|| {
            for entry in stats
                .iter()
                .filter(|entry| entry.persistent && !entry.autonomous)
            {
                Self::persist_column_stats(
                    session.catalog.as_ref(),
                    &entry.table_name,
                    &entry.stats,
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => session.backend.commit_transaction(),
            Err(error) => match session.backend.rollback_transaction() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(StorageBackendError::Other(format!(
                    "restore nontransactional ANALYZE statistics failed: {error}; rollback also failed: {rollback_error}"
                ))),
            },
        }
    }

    pub(super) fn apply_nontransactional_column_stats(
        &self,
        stats: &NontransactionalColumnStats,
    ) -> StorageBackendResult<()> {
        for entry in stats {
            let Some(table) = self.try_table(&entry.table_name)? else {
                continue;
            };
            *table.column_stats.write() = entry.stats.clone();
            table
                .column_stats_loaded
                .store(true, std::sync::atomic::Ordering::Release);
            table
                .column_stats_dirty
                .store(false, std::sync::atomic::Ordering::Release);
            if entry.persistent {
                self.row_locks
                    .publish_column_stats(entry.table_name.clone(), entry.stats.clone());
            }
        }
        Ok(())
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
            let persistence = self.durable.sequence_persistence.read();
            values
                .iter()
                .filter(|(relation, _)| sequences.contains_key(*relation))
                .filter(|(relation, _)| {
                    persistence.get(*relation).copied().unwrap_or_default()
                        != uqa_sql::ast::RelationPersistence::Temporary
                })
                .map(|(relation, value)| (relation.qualified_name(), *value))
                .collect::<Vec<_>>()
        };
        if persistent.is_empty() {
            return Ok(());
        }
        let persist = |catalog: &dyn uqa_storage::CatalogFacade| -> StorageBackendResult<()> {
            for (name, value) in &persistent {
                if catalog.set_sequence_value(name, *value)?.is_none() {
                    return Err(StorageBackendError::Other(format!(
                        "sequence `{name}` disappeared while restoring its nontransactional value"
                    )));
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
        let mut session = self.session.state.write();
        for (relation, value) in values {
            let Some(sequence) = sequences.get_mut(relation) else {
                continue;
            };
            sequence.current = *value;
            sequence.called = true;
            session.sequence_currvals.insert(relation.clone(), *value);
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

    pub(super) fn nontransactional_column_stats_after_rollback(
        &self,
        stats: &NontransactionalColumnStats,
        relation_states: &TransactionRelationStates,
    ) -> NontransactionalColumnStats {
        for entry in stats {
            self.row_locks.invalidate_column_stats(&entry.table_name);
        }
        stats
            .iter()
            .filter(|entry| {
                relation_states.iter().any(|(relation, lifecycle_id)| {
                    relation.qualified_name() == entry.table_name
                        && *lifecycle_id == entry.table_lifecycle_id
                })
            })
            .cloned()
            .collect()
    }
}
