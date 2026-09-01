//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL savepoint state and backend savepoint coordination.

use super::{
    BackendTransactionMode, Engine, SQLError, StorageSavepointId, TransactionFrame,
    TransactionSavepoint, TransactionStatus,
};

impl Engine {
    /// A deferred outer frame still runs a backend read transaction, which cannot carry backend savepoints. Savepoints are then recorded on the frame only; promotion to a writer recreates every recorded savepoint on the write transaction, so `PostgreSQL`'s fresh READ COMMITTED snapshot per statement survives `SAVEPOINT` and nested `BEGIN`.
    pub(super) fn backend_savepoints_deferred(stack: &[TransactionFrame]) -> bool {
        stack
            .first()
            .is_some_and(|frame| frame.backend_mode == BackendTransactionMode::Deferred)
    }

    pub(super) fn save_transaction_savepoint(
        &self,
        stack: &mut [TransactionFrame],
        name: String,
    ) -> Result<(), SQLError> {
        if stack.is_empty() {
            return Err(SQLError::Routine {
                sqlstate: "25P01".into(),
                message: "SAVEPOINT can only be used in transaction blocks".into(),
            });
        }
        let session_snapshot = self.snapshot_session_state();
        let data_snapshot = self.snapshot_transaction_data()?;
        let relation_states_at_begin = self.transaction_relation_states();
        let deferred = Self::backend_savepoints_deferred(stack);
        let storage_savepoint = StorageSavepointId::allocate();
        let frame = stack.last_mut().ok_or_else(|| {
            SQLError::Internal("SAVEPOINT lost its checked transaction frame".into())
        })?;
        if let Some(backend) = self.storage.backend.as_ref().filter(|_| !deferred) {
            backend
                .savepoint(storage_savepoint)
                .map_err(|err| Self::storage_tx_error("SAVEPOINT", &err))?;
        }
        let keep_mark = frame.lock_mark;
        frame.lock_mark = frame.next_lock_mark;
        frame.next_lock_mark = frame.next_lock_mark.saturating_add(1);
        let row_changes = frame.row_changes.clone();
        let deferred_foreign_key_checks = frame.deferred_foreign_key_checks.clone();
        let deferred_constraint_trigger_events = frame.deferred_constraint_trigger_events.clone();
        let constraint_modes = frame.constraint_modes.clone();
        frame.savepoints.push(TransactionSavepoint {
            name,
            storage_savepoint,
            intent: frame.intent,
            characteristics: frame.characteristics,
            session_snapshot,
            data_snapshot,
            relation_states_at_begin,
            dirty: self.transaction_dirty_state(),
            lock_mark: keep_mark,
            row_changes,
            deferred_foreign_key_checks,
            deferred_constraint_trigger_events,
            constraint_modes,
        });
        frame.xid_levels.push(None);
        Ok(())
    }

    pub(super) fn release_transaction_savepoint(
        &self,
        stack: &mut [TransactionFrame],
        name: &str,
    ) -> Result<(), SQLError> {
        let deferred = Self::backend_savepoints_deferred(stack);
        let frame = stack.last_mut().ok_or_else(|| SQLError::Routine {
            sqlstate: "25P01".into(),
            message: "RELEASE SAVEPOINT can only be used in transaction blocks".into(),
        })?;
        let position = frame
            .savepoints
            .iter()
            .rposition(|savepoint| savepoint.name == name)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "3B001".into(),
                message: format!("savepoint \"{name}\" does not exist"),
            })?;
        let storage_savepoint = frame.savepoints[position].storage_savepoint;
        let intent = frame.savepoints[position].intent;
        let characteristics = frame.savepoints[position].characteristics;
        if let Some(backend) = self.storage.backend.as_ref().filter(|_| !deferred) {
            backend
                .release_savepoint(storage_savepoint)
                .map_err(|err| Self::storage_tx_error("RELEASE SAVEPOINT", &err))?;
        }
        frame.intent = intent;
        frame.characteristics = characteristics;
        frame.savepoints.truncate(position);
        frame.xid_levels.truncate(position + 1);
        Ok(())
    }

    pub(super) fn rollback_to_transaction_savepoint(
        &self,
        stack: &mut [TransactionFrame],
        name: &str,
    ) -> Result<(), SQLError> {
        let position = stack
            .last()
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "25P01".into(),
                message: "ROLLBACK TO SAVEPOINT can only be used in transaction blocks".into(),
            })?
            .savepoints
            .iter()
            .rposition(|savepoint| savepoint.name == name)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "3B001".into(),
                message: format!("savepoint \"{name}\" does not exist"),
            })?;
        let rollback_relation_states = stack
            .last()
            .and_then(|frame| frame.savepoints.get(position))
            .map(|savepoint| savepoint.relation_states_at_begin.clone())
            .unwrap_or_default();
        let nontransactional_column_stats =
            self.retain_nontransactional_stats_for_rollback(stack, &rollback_relation_states);
        let deferred = Self::backend_savepoints_deferred(stack);
        let frame = stack.last_mut().ok_or_else(|| SQLError::Routine {
            sqlstate: "25P01".into(),
            message: "ROLLBACK TO SAVEPOINT can only be used in transaction blocks".into(),
        })?;
        let nontransactional_sequence_values = frame.nontransactional_sequence_values.clone();
        let storage_savepoint = frame.savepoints[position].storage_savepoint;
        if let Some(backend) = self.storage.backend.as_ref().filter(|_| !deferred) {
            backend
                .rollback_to_savepoint(storage_savepoint)
                .map_err(|err| Self::storage_tx_error("ROLLBACK TO SAVEPOINT", &err))?;
        }
        let mut cleanup_errors = Vec::new();
        let savepoint = &frame.savepoints[position];
        if let Some(snapshot) = savepoint.data_snapshot.as_ref() {
            if let Err(error) = self.restore_transaction_data(snapshot) {
                cleanup_errors.push(format!("memory restore: {error}"));
            }
        }
        self.restore_transaction_dirty_state(savepoint.dirty);
        if let Err(error) = self.persist_nontransactional_column_stats_after_rollback(
            &nontransactional_column_stats,
            false,
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
        self.restore_session_state_preserving_sequences(
            &savepoint.session_snapshot,
            &nontransactional_sequence_values,
            false,
            &mut cleanup_errors,
        );
        let keep_mark = savepoint.lock_mark;
        frame.row_changes.clone_from(&savepoint.row_changes);
        frame
            .deferred_foreign_key_checks
            .clone_from(&savepoint.deferred_foreign_key_checks);
        frame
            .deferred_constraint_trigger_events
            .clone_from(&savepoint.deferred_constraint_trigger_events);
        frame
            .constraint_modes
            .clone_from(&savepoint.constraint_modes);
        frame.intent = savepoint.intent;
        frame.characteristics = savepoint.characteristics;
        frame.savepoints.truncate(position + 1);
        frame.xid_levels.truncate(position + 2);
        let current_xid = frame.xid_levels.last_mut().ok_or_else(|| {
            SQLError::Internal("ROLLBACK TO SAVEPOINT lost its transaction XID level".into())
        })?;
        *current_xid = None;
        self.row_locks
            .release_mark_above(self.session_id, keep_mark);
        frame.lock_mark = frame.next_lock_mark;
        frame.next_lock_mark = frame.next_lock_mark.saturating_add(1);
        frame.status = TransactionStatus::Active;
        if cleanup_errors.is_empty() {
            Ok(())
        } else {
            Err(SQLError::Internal(format!(
                "ROLLBACK TO SAVEPOINT completed but engine state restoration failed: {}",
                cleanup_errors.join("; ")
            )))
        }
    }
}
