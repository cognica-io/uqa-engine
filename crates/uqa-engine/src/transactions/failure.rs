//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction failure recovery and cleanup ownership.

use super::{
    ConstraintModeState, Engine, EngineDataSnapshot, SQLError, SessionStateSnapshot,
    StorageBackendError, StorageBackendResult, StorageSavepointId, TransactionCharacteristicsState,
    TransactionDirtyState, TransactionFrame, TransactionRowChange, TransactionStatus,
};

pub(super) fn panic_description(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

struct StatementAbortSnapshot {
    storage_savepoint: Option<StorageSavepointId>,
    session: SessionStateSnapshot,
    data: Option<EngineDataSnapshot>,
    dirty: TransactionDirtyState,
    keep_mark: Option<u32>,
    row_changes: Vec<TransactionRowChange>,
    statistics_changes: crate::statistics::StatisticsChanges,
    deferred_foreign_key_checks: Vec<crate::DeferredForeignKeyCheck>,
    deferred_constraint_trigger_events:
        Vec<uqa_execution::mutation::triggers::DeferredConstraintTriggerEvent>,
    pending_listen_actions: Vec<crate::PendingListenAction>,
    pending_notifications: Vec<crate::PendingNotification>,
    constraint_modes: ConstraintModeState,
    characteristics: TransactionCharacteristicsState,
    first_snapshot_set: bool,
}

fn statement_abort_snapshot(frame: &TransactionFrame) -> StatementAbortSnapshot {
    if let Some(savepoint) = frame.savepoints.last() {
        return StatementAbortSnapshot {
            storage_savepoint: Some(savepoint.storage_savepoint),
            session: savepoint.session_snapshot.clone(),
            data: savepoint.data_snapshot.clone(),
            dirty: savepoint.dirty,
            keep_mark: Some(savepoint.lock_mark),
            row_changes: savepoint.row_changes.clone(),
            statistics_changes: savepoint.statistics_changes.clone(),
            deferred_foreign_key_checks: savepoint.deferred_foreign_key_checks.clone(),
            deferred_constraint_trigger_events: savepoint
                .deferred_constraint_trigger_events
                .clone(),
            pending_listen_actions: savepoint.pending_listen_actions.clone(),
            pending_notifications: savepoint.pending_notifications.clone(),
            constraint_modes: savepoint.constraint_modes.clone(),
            characteristics: savepoint.characteristics,
            // PostgreSQL's FirstSnapshotSet belongs to the top transaction, not to a subtransaction or savepoint. Once any statement has acquired a snapshot, error recovery must never make it false.
            first_snapshot_set: frame.first_snapshot_set,
        };
    }
    StatementAbortSnapshot {
        storage_savepoint: None,
        session: frame.session_snapshot.clone(),
        data: frame.data_snapshot.clone(),
        dirty: frame.dirty_at_begin,
        keep_mark: frame
            .storage_savepoint
            .as_ref()
            .map(|_| frame.begin_lock_mark.saturating_sub(1)),
        row_changes: Vec::new(),
        statistics_changes: crate::statistics::StatisticsChanges::new(),
        deferred_foreign_key_checks: Vec::new(),
        deferred_constraint_trigger_events: Vec::new(),
        pending_listen_actions: Vec::new(),
        pending_notifications: Vec::new(),
        constraint_modes: ConstraintModeState::default(),
        characteristics: frame.characteristics,
        first_snapshot_set: frame.first_snapshot_set,
    }
}

impl Engine {
    /// Retain uncertain completion when adding cleanup context, including while the caller holds the transaction stack.
    pub(super) fn rollback_cleanup_error(rollback: &SQLError, detail: String) -> SQLError {
        if rollback.sqlstate() == Some("08007") {
            SQLError::Routine {
                sqlstate: "08007".into(),
                message: detail,
            }
        } else {
            SQLError::Internal(detail)
        }
    }

    pub(crate) fn abort_sql_transaction_after_error(&self, error: SQLError) -> SQLError {
        let cleanup_errors = self.abort_transaction_after_failure();
        if cleanup_errors.is_empty() {
            error
        } else {
            self.transaction_abort_cleanup_error(format!(
                "{error}; transaction abort cleanup failed: {}",
                cleanup_errors.join("; ")
            ))
        }
    }

    pub(super) fn run_existing_transaction_operation<R, E: std::fmt::Display>(
        &self,
        operation: impl FnOnce() -> Result<R, E>,
        map_cleanup_error: impl Fn(SQLError) -> E,
    ) -> Result<R, E> {
        self.finish_existing_transaction_operation(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)),
            map_cleanup_error,
        )
    }

    pub(super) fn finish_existing_transaction_operation<R, E: std::fmt::Display>(
        &self,
        result: std::thread::Result<Result<R, E>>,
        map_cleanup_error: impl Fn(SQLError) -> E,
    ) -> Result<R, E> {
        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                let cleanup_errors = self.abort_transaction_after_failure();
                if cleanup_errors.is_empty() {
                    Err(error)
                } else {
                    Err(map_cleanup_error(self.transaction_abort_cleanup_error(
                        format!(
                            "{error}; transaction abort cleanup failed: {}",
                            cleanup_errors.join("; ")
                        ),
                    )))
                }
            }
            Err(payload) => {
                let cleanup_errors = self.abort_transaction_after_failure();
                if cleanup_errors.is_empty() {
                    std::panic::resume_unwind(payload)
                } else {
                    Err(map_cleanup_error(self.transaction_abort_cleanup_error(
                        format!(
                            "transaction abort after panic failed: {}; original panic: {}",
                            cleanup_errors.join("; "),
                            panic_description(payload.as_ref())
                        ),
                    )))
                }
            }
        }
    }

    fn transaction_abort_cleanup_error(&self, detail: String) -> SQLError {
        if let Some(transaction) = self.pending_transaction_completion() {
            Self::pending_completion_error(transaction, detail)
        } else {
            SQLError::Internal(detail)
        }
    }

    fn abort_transaction_after_failure(&self) -> Vec<String> {
        let _statement = self.runtime.statement_gate.lock();
        let mut stack = self.session.transactions.lock();
        let Some(frame) = stack.last() else {
            return Vec::new();
        };
        if frame.status != TransactionStatus::Active {
            return Vec::new();
        }

        let rollback_state = statement_abort_snapshot(frame);
        let storage_savepoint = rollback_state.storage_savepoint.or(frame.storage_savepoint);
        let outer_frame = &stack[0];
        let nontransactional_sequence_values = outer_frame.nontransactional_sequence_values.clone();
        let mut cleanup_errors = Vec::new();
        let backend_aborted = match self
            .rollback_failed_statement_backend(&stack, storage_savepoint)
        {
            Ok(aborted) => aborted,
            Err(rollback_error) => {
                cleanup_errors.push(format!("storage rollback: {rollback_error}"));
                let pending =
                    Self::retain_pending_completion(&mut stack, &rollback_error, true).is_some();
                if pending
                    || storage_savepoint.is_some()
                    || self
                        .storage
                        .backend
                        .as_ref()
                        .is_some_and(|backend| backend.in_transaction())
                {
                    if !pending {
                        stack.last_mut().expect("retained frame").status =
                            TransactionStatus::Failed;
                    }
                    return cleanup_errors;
                }
                true
            }
        };

        self.restore_graph_transaction_overlay(&rollback_state.session);
        if let Some(snapshot) = rollback_state.data.as_ref() {
            if let Err(restore_error) = self.restore_transaction_data(snapshot) {
                cleanup_errors.push(format!("memory restore: {restore_error}"));
            }
        }
        self.restore_transaction_dirty_state(rollback_state.dirty);
        if let Err(restore_error) = self.reload_persistent_value_indexes() {
            cleanup_errors.push(format!("btree restore: {restore_error}"));
        }
        if self.storage.backend.is_some() {
            if let Err(restore_error) = self.reload_table_catalog_after_rollback() {
                cleanup_errors.push(format!("table catalog restore: {restore_error}"));
            }
            if let Err(restore_error) = self.reload_catalog_registries_after_rollback() {
                cleanup_errors.push(format!("registry restore: {restore_error}"));
            }
        }
        self.restore_session_state_preserving_sequences(
            &rollback_state.session,
            &nontransactional_sequence_values,
            backend_aborted,
            &mut cleanup_errors,
        );
        self.release_aborted_statement_locks(rollback_state.keep_mark);
        if let Some(frame) = stack.last_mut() {
            frame.status = if backend_aborted {
                TransactionStatus::FailedBackendAborted
            } else {
                TransactionStatus::Failed
            };
            frame.row_changes = rollback_state.row_changes;
            frame.statistics_changes = rollback_state.statistics_changes;
            frame.deferred_foreign_key_checks = rollback_state.deferred_foreign_key_checks;
            frame.deferred_constraint_trigger_events =
                rollback_state.deferred_constraint_trigger_events;
            frame.pending_listen_actions = rollback_state.pending_listen_actions;
            frame.pending_notifications = rollback_state.pending_notifications;
            frame.constraint_modes = rollback_state.constraint_modes;
            frame.characteristics = rollback_state.characteristics;
            frame.first_snapshot_set = rollback_state.first_snapshot_set;
        }
        cleanup_errors
    }

    fn rollback_failed_statement_backend(
        &self,
        stack: &[TransactionFrame],
        storage_savepoint: Option<StorageSavepointId>,
    ) -> StorageBackendResult<bool> {
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(false);
        };
        // Nested frames retain the outer writes and locks. Deferred savepoints have no backend state until writer promotion.
        if let Some(savepoint) = storage_savepoint {
            if !Self::backend_savepoints_deferred(stack) {
                backend.rollback_to_savepoint(savepoint)?;
            }
            return Ok(false);
        }
        backend.rollback_transaction()?;
        if backend.in_transaction() {
            return Err(StorageBackendError::Other(
                "storage rollback did not end the transaction; transaction state is retained"
                    .into(),
            ));
        }
        Ok(true)
    }

    fn release_aborted_statement_locks(&self, keep_mark: Option<u32>) {
        if let Some(mark) = keep_mark {
            self.row_locks.release_mark_above(self.session_id, mark);
        } else {
            self.row_locks.release_session(self.session_id);
        }
    }
}

pub(super) fn failed_transaction_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "25P02".into(),
        message: "current transaction is aborted, commands ignored until end of transaction block"
            .into(),
    }
}
