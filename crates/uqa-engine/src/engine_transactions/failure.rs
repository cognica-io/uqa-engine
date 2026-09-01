//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction failure recovery and cleanup ownership.

use super::{
    ConstraintModeState, Engine, EngineDataSnapshot, SQLError, SessionStateSnapshot,
    StorageSavepointId, TransactionCharacteristicsState, TransactionDirtyState, TransactionFrame,
    TransactionIntent, TransactionRelationStates, TransactionRowChange, TransactionStatus,
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
    relation_states: TransactionRelationStates,
    dirty: TransactionDirtyState,
    keep_mark: Option<u32>,
    row_changes: Vec<TransactionRowChange>,
    deferred_foreign_key_checks: Vec<crate::DeferredForeignKeyCheck>,
    deferred_constraint_trigger_events: Vec<crate::sql::DeferredConstraintTriggerEvent>,
    constraint_modes: ConstraintModeState,
    intent: TransactionIntent,
    characteristics: TransactionCharacteristicsState,
    first_snapshot_set: bool,
}

fn statement_abort_snapshot(frame: &TransactionFrame) -> StatementAbortSnapshot {
    if let Some(savepoint) = frame.savepoints.last() {
        return StatementAbortSnapshot {
            storage_savepoint: Some(savepoint.storage_savepoint),
            session: savepoint.session_snapshot.clone(),
            data: savepoint.data_snapshot.clone(),
            relation_states: savepoint.relation_states_at_begin.clone(),
            dirty: savepoint.dirty,
            keep_mark: Some(savepoint.lock_mark),
            row_changes: savepoint.row_changes.clone(),
            deferred_foreign_key_checks: savepoint.deferred_foreign_key_checks.clone(),
            deferred_constraint_trigger_events: savepoint
                .deferred_constraint_trigger_events
                .clone(),
            constraint_modes: savepoint.constraint_modes.clone(),
            intent: savepoint.intent,
            characteristics: savepoint.characteristics,
            // PostgreSQL's FirstSnapshotSet belongs to the top transaction, not to a subtransaction or savepoint. Once any statement has acquired a snapshot, error recovery must never make it false.
            first_snapshot_set: frame.first_snapshot_set,
        };
    }
    StatementAbortSnapshot {
        storage_savepoint: None,
        session: frame.session_snapshot.clone(),
        data: frame.data_snapshot.clone(),
        relation_states: frame.relation_states_at_begin.clone(),
        dirty: frame.dirty_at_begin,
        keep_mark: frame
            .storage_savepoint
            .as_ref()
            .map(|_| frame.begin_lock_mark.saturating_sub(1)),
        row_changes: Vec::new(),
        deferred_foreign_key_checks: Vec::new(),
        deferred_constraint_trigger_events: Vec::new(),
        constraint_modes: ConstraintModeState::default(),
        intent: frame.intent,
        characteristics: frame.characteristics,
        first_snapshot_set: frame.first_snapshot_set,
    }
}

impl Engine {
    pub(crate) fn abort_sql_transaction_after_error(&self, error: SQLError) -> SQLError {
        let _statement = self.runtime.statement_gate.lock();
        let mut stack = self.session.transactions.lock();
        let Some(frame) = stack.last() else {
            return error;
        };
        if frame.status != TransactionStatus::Active {
            return error;
        }

        let rollback_state = statement_abort_snapshot(frame);
        let frame_storage_savepoint = frame.storage_savepoint;
        let outer_frame = &stack[0];
        let raw_nontransactional_column_stats = outer_frame.nontransactional_column_stats.clone();
        let nontransactional_sequence_values = outer_frame.nontransactional_sequence_values.clone();
        let nontransactional_column_stats = self.nontransactional_column_stats_after_rollback(
            &raw_nontransactional_column_stats,
            &rollback_state.relation_states,
        );
        if let Some(frame) = stack.first_mut() {
            frame
                .nontransactional_column_stats
                .clone_from(&nontransactional_column_stats);
        }
        // A nested frame owns a backend savepoint of its own; aborting the statement rolls the storage back to that savepoint so the outer frames' writes and locks survive, exactly like a PostgreSQL subtransaction abort. Only the outermost frame aborts the whole backend transaction.
        let savepoints_deferred = Self::backend_savepoints_deferred(&stack);
        let mut cleanup_errors = Vec::new();
        let mut backend_aborted = false;

        if let Some(backend) = self.storage.backend.as_ref() {
            // A deferred outer transaction has written nothing to storage, so its backend savepoints exist only logically and there is nothing to roll back at the backend for a savepoint or nested frame; the outermost abort still ends the read transaction.
            let rollback = if rollback_state.storage_savepoint.is_some()
                || frame_storage_savepoint.is_some()
            {
                if savepoints_deferred {
                    Ok(())
                } else if let Some(storage_savepoint) = rollback_state.storage_savepoint {
                    backend.rollback_to_savepoint(storage_savepoint)
                } else if let Some(frame_savepoint) = frame_storage_savepoint {
                    backend.rollback_to_savepoint(frame_savepoint)
                } else {
                    Ok(())
                }
            } else {
                backend_aborted = true;
                backend.rollback_transaction()
            };
            if let Err(rollback_error) = rollback {
                cleanup_errors.push(format!("storage rollback: {rollback_error}"));
            }
        }

        if let Some(snapshot) = rollback_state.data.as_ref() {
            if let Err(restore_error) = self.restore_transaction_data(snapshot) {
                cleanup_errors.push(format!("memory restore: {restore_error}"));
            }
        }
        self.restore_transaction_dirty_state(rollback_state.dirty);
        if let Err(restore_error) = self.persist_nontransactional_column_stats_after_rollback(
            &nontransactional_column_stats,
            backend_aborted,
        ) {
            cleanup_errors.push(format!("ANALYZE statistics restore: {restore_error}"));
        }
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
        if let Err(restore_error) =
            self.apply_nontransactional_column_stats(&nontransactional_column_stats)
        {
            cleanup_errors.push(format!("ANALYZE statistics cache restore: {restore_error}"));
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
            frame.deferred_foreign_key_checks = rollback_state.deferred_foreign_key_checks;
            frame.deferred_constraint_trigger_events =
                rollback_state.deferred_constraint_trigger_events;
            frame.constraint_modes = rollback_state.constraint_modes;
            frame.intent = rollback_state.intent;
            frame.characteristics = rollback_state.characteristics;
            frame.first_snapshot_set = rollback_state.first_snapshot_set;
        }
        transaction_abort_result(error, &cleanup_errors)
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

fn transaction_abort_result(error: SQLError, cleanup_errors: &[String]) -> SQLError {
    if cleanup_errors.is_empty() {
        error
    } else {
        SQLError::Internal(format!(
            "{error}; transaction abort cleanup failed: {}",
            cleanup_errors.join("; ")
        ))
    }
}
