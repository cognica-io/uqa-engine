//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coordination of transaction frames and their storage snapshots.
//!
//! Memory snapshot acquisition is ordered by the statement gate, the table registry and its per-table state, the durable registries (`graphs`, `models`, `scoring_params`, `views`, `catalog_indexes`, `database_security`, `schemas`, `path_indexes`, `sequences`, `sequence_object_ids`, `sequence_persistence`, `sequence_security`, `named_analyzers`, `table_field_analyzers`, `foreign_servers`, `foreign_tables`, `foreign_table_security`, `sql_user_functions`, `roles`, `role_memberships`, `triggers`, and `rules`), and finally in-memory FDW rows. Durable restore uses the same registry order. Callers must enter through this coordinator instead of holding an individual registry lock across snapshot or restore.

use super::{
    failed_transaction_error, BackendTransactionMode, ConstraintModeState, Engine,
    EngineDataSnapshot, NontransactionalColumnStats, NontransactionalSequenceValues, SQLError,
    SessionStateSnapshot, StorageBackendError, StorageSavepointId, TransactionCharacteristicsState,
    TransactionDirtyState, TransactionFrame, TransactionFrameKind, TransactionIntent,
    TransactionRelationStates, TransactionStatus,
};

impl Engine {
    pub(super) fn transaction_dirty_state(&self) -> TransactionDirtyState {
        TransactionDirtyState {
            table_data: self
                .epochs
                .table_data
                .dirty
                .load(std::sync::atomic::Ordering::Acquire),
            table_catalog: self
                .epochs
                .table_catalog
                .dirty
                .load(std::sync::atomic::Ordering::Acquire),
            catalog_registry: self
                .epochs
                .catalog_registry
                .dirty
                .load(std::sync::atomic::Ordering::Acquire),
        }
    }

    pub(super) fn transaction_relation_states(&self) -> TransactionRelationStates {
        self.storage
            .tables
            .read()
            .iter()
            .map(|(relation, table)| (relation.clone(), table.lifecycle_id()))
            .collect()
    }

    pub(super) fn restore_transaction_dirty_state(&self, state: TransactionDirtyState) {
        self.epochs
            .table_data
            .dirty
            .store(state.table_data, std::sync::atomic::Ordering::Release);
        self.epochs
            .table_catalog
            .dirty
            .store(state.table_catalog, std::sync::atomic::Ordering::Release);
        self.epochs
            .catalog_registry
            .dirty
            .store(state.catalog_registry, std::sync::atomic::Ordering::Release);
    }

    pub fn begin(&self) -> Result<(), SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let mut stack = self.session.transactions.lock();
        if stack
            .last()
            .is_some_and(|frame| frame.status != TransactionStatus::Active)
        {
            return Err(failed_transaction_error());
        }
        let characteristics = self.transaction_characteristics_for_begin(
            &stack,
            uqa_sql::ast::TransactionCharacteristics::default(),
        );
        self.begin_transaction_frame(
            &mut stack,
            characteristics.read_only,
            true,
            TransactionFrameKind::ExplicitBlock,
            characteristics,
        )
    }

    /// Commit the topmost transaction frame.
    pub fn commit(&self) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit)
    }

    /// Roll back the topmost transaction frame.
    pub fn rollback(&self) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Rollback)
    }

    /// Mark a savepoint inside the current transaction.
    pub fn savepoint(&self, name: &str) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Savepoint(name.to_string()))
    }

    /// Release a savepoint.
    pub fn release_savepoint(&self, name: &str) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::ReleaseSavepoint(
            name.to_string(),
        ))
    }

    /// Roll back to a named savepoint.
    pub fn rollback_to_savepoint(&self, name: &str) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::RollbackToSavepoint(
            name.to_string(),
        ))
    }

    /// Number of currently-open transaction frames (`BEGIN` count
    /// minus `COMMIT/ROLLBACK` count). Useful for assertions in tests
    /// and for status displays in the CLI.
    pub fn transaction_depth(&self) -> usize {
        self.session_execution_view().transaction_depth()
    }

    pub(crate) fn in_transaction_block(&self) -> bool {
        self.session
            .transactions
            .lock()
            .last()
            .is_some_and(|frame| !frame.implicit_statement)
    }

    pub(crate) fn in_explicit_transaction_block(&self) -> bool {
        self.session
            .transactions
            .lock()
            .last()
            .is_some_and(|frame| frame.explicit_transaction_block)
    }

    /// Tear down engine state cleanly, rolling back open transaction frames
    /// and clearing registries.
    /// The engine value can no longer be used afterwards in a
    /// well-defined sense; idiomatic Rust drops the value at scope
    /// exit, but this method exists for API compatibility
    /// reference and for explicit shutdown ordering.
    pub fn close(&self) -> Result<(), SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        // Use the ordinary state machine for every frame. Clearing the stack
        // before asking SQLite to roll back loses the session snapshot and can
        // leave dirty flags/caches inconsistent when physical cleanup fails.
        while self.transaction_depth() != 0 {
            self.rollback().map_err(|error| {
                SQLError::Internal(format!("close: rollback open transaction failed: {error}"))
            })?;
        }
        // Logical catalog and runtime registries are database state shared by
        // sibling sessions. Closing one session must not erase them from the
        // sessions that remain alive.
        Ok(())
    }

    pub(crate) fn ensure_transaction_usable(&self) -> Result<(), SQLError> {
        if self
            .session
            .transactions
            .lock()
            .last()
            .is_some_and(|frame| frame.status != TransactionStatus::Active)
        {
            return Err(failed_transaction_error());
        }
        Ok(())
    }

    pub(crate) fn begin_implicit_statement_transaction(
        &self,
        read_only: bool,
    ) -> Result<(), SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let mut stack = self.session.transactions.lock();
        if !stack.is_empty() {
            return Err(SQLError::Internal(
                "implicit statement transaction started inside an explicit transaction".into(),
            ));
        }
        let characteristics = self.default_transaction_characteristics();
        self.begin_transaction_frame(
            &mut stack,
            read_only || characteristics.read_only,
            true,
            TransactionFrameKind::ImplicitStatement,
            characteristics,
        )
    }

    /// Start the implicit transaction segment owned by a multi-statement simple-query message.
    pub fn begin_simple_query_transaction(&self) -> Result<(), SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let mut stack = self.session.transactions.lock();
        if !stack.is_empty() {
            return Err(SQLError::Internal(
                "implicit transaction block started inside another transaction".into(),
            ));
        }
        let characteristics = self.default_transaction_characteristics();
        self.begin_transaction_frame(
            &mut stack,
            characteristics.read_only,
            true,
            TransactionFrameKind::SimpleQuery,
            characteristics,
        )
    }

    /// Promote the current simple-query transaction to an explicit block after `BEGIN`.
    pub fn promote_simple_query_transaction(&self) -> Result<(), SQLError> {
        let mut stack = self.session.transactions.lock();
        let frame = stack.last_mut().ok_or_else(|| {
            SQLError::Internal("promote implicit transaction without an open frame".into())
        })?;
        frame.implicit_statement = false;
        frame.explicit_transaction_block = true;
        Ok(())
    }

    pub(super) fn begin_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
        read_only: bool,
        defer_write_lock: bool,
        kind: TransactionFrameKind,
        mut characteristics: TransactionCharacteristicsState,
    ) -> Result<(), SQLError> {
        // A PostgreSQL subtransaction cannot relax the enclosing transaction's read-only mode. This is also the invariant relied on by PL/pgSQL EXCEPTION blocks, which use nested engine frames as subtransactions.
        if stack
            .last()
            .is_some_and(|frame| frame.characteristics.read_only)
        {
            characteristics.read_only = true;
        }
        let read_only = read_only || characteristics.read_only;
        let session_snapshot = self.snapshot_session_state();
        let (storage_savepoint, data_snapshot, snapshot_change_baseline) = if stack.is_empty() {
            let (data_snapshot, baseline) = self.begin_outer_transaction_snapshot(
                read_only,
                defer_write_lock,
                &session_snapshot,
            )?;
            (None, data_snapshot, baseline)
        } else {
            let baseline = stack[0].snapshot_change_baseline;
            let savepoint = StorageSavepointId::allocate();
            if let Some(backend) = self.storage.backend.as_ref() {
                if !Self::backend_savepoints_deferred(stack) {
                    backend
                        .savepoint(savepoint)
                        .map_err(|err| Self::storage_tx_error("nested BEGIN savepoint", &err))?;
                }
                (Some(savepoint), None, baseline)
            } else {
                (Some(savepoint), self.snapshot_transaction_data()?, baseline)
            }
        };
        let (constraint_modes, deferred_foreign_key_checks, deferred_constraint_trigger_events) =
            stack.last().map_or_else(
                || (ConstraintModeState::default(), Vec::new(), Vec::new()),
                |frame| {
                    (
                        frame.constraint_modes.clone(),
                        frame.deferred_foreign_key_checks.clone(),
                        frame.deferred_constraint_trigger_events.clone(),
                    )
                },
            );
        let (lock_mark, next_lock_mark) = stack.last_mut().map_or((0, 1), |frame| {
            let lock_mark = frame.next_lock_mark;
            frame.next_lock_mark = frame.next_lock_mark.saturating_add(1);
            (lock_mark, frame.next_lock_mark)
        });
        let backend_mode = if stack.is_empty() && defer_write_lock && self.storage.backend.is_some()
        {
            BackendTransactionMode::Deferred
        } else {
            BackendTransactionMode::Writer
        };
        let relation_states_at_begin = self.transaction_relation_states();
        let (implicit_statement, explicit_transaction_block) = match kind {
            TransactionFrameKind::ExplicitBlock => (false, true),
            TransactionFrameKind::ImplicitStatement => (true, false),
            TransactionFrameKind::SimpleQuery => (false, false),
        };
        stack.push(TransactionFrame {
            implicit_statement,
            explicit_transaction_block,
            storage_savepoint,
            intent: if read_only {
                TransactionIntent::ReadOnly
            } else {
                TransactionIntent::ReadWrite
            },
            backend_mode,
            status: TransactionStatus::Active,
            characteristics,
            first_snapshot_set: false,
            fixed_snapshot: None,
            fixed_catalog_baseline: None,
            xid_levels: vec![None],
            savepoints: Vec::new(),
            session_snapshot,
            data_snapshot,
            relation_states_at_begin,
            dirty_at_begin: self.transaction_dirty_state(),
            begin_lock_mark: lock_mark,
            lock_mark,
            next_lock_mark,
            snapshot_change_baseline,
            row_changes: Vec::new(),
            deferred_foreign_key_checks,
            deferred_constraint_trigger_events,
            constraint_modes,
            nontransactional_column_stats: NontransactionalColumnStats::new(),
            nontransactional_sequence_values: NontransactionalSequenceValues::new(),
        });
        self.update_statement_row_lock_baseline(snapshot_change_baseline);
        Ok(())
    }

    /// Return the transaction ID that creates a new tuple version. IDs are allocated lazily, and a first write below a savepoint allocates every missing ancestor ID before the active subtransaction ID, matching `PostgreSQL`'s top-XID/sub-XID hierarchy. Direct in-memory APIs intentionally omit a heavyweight transaction snapshot when no frame is open; each such autocommit row write still receives its own durable XID.
    pub(crate) fn tuple_version_xid(&self) -> Result<u32, SQLError> {
        let mut stack = self.session.transactions.lock();
        if stack.is_empty() {
            drop(stack);
            return self.row_locks.allocate_transaction_xid();
        }
        for frame in stack.iter_mut() {
            for xid in &mut frame.xid_levels {
                if xid.is_none() {
                    *xid = Some(self.row_locks.allocate_transaction_xid()?);
                }
            }
        }
        stack
            .last()
            .and_then(|frame| frame.xid_levels.last())
            .copied()
            .flatten()
            .ok_or_else(|| SQLError::Internal("transaction XID path is empty".into()))
    }

    fn begin_outer_transaction_snapshot(
        &self,
        read_only: bool,
        defer_write_lock: bool,
        session_snapshot: &SessionStateSnapshot,
    ) -> Result<
        (
            Option<EngineDataSnapshot>,
            crate::row_locks::RowChangeBaseline,
        ),
        SQLError,
    > {
        let Some(backend) = self.storage.backend.as_ref() else {
            let snapshot_gate = self
                .row_locks
                .begin_change_snapshot(&self.runtime.cancellation)?;
            self.synchronize_table_catalog()
                .map_err(|err| Self::storage_tx_error("BEGIN table catalog refresh", &err))?;
            self.synchronize_table_data()
                .map_err(|err| Self::storage_tx_error("BEGIN table data refresh", &err))?;
            self.synchronize_catalog_registries()
                .map_err(|err| Self::storage_tx_error("BEGIN registry refresh", &err))?;
            return Ok((self.snapshot_transaction_data()?, snapshot_gate.baseline()?));
        };
        let snapshot_gate = if read_only || defer_write_lock {
            let gate = self
                .row_locks
                .begin_change_snapshot(&self.runtime.cancellation)?;
            backend
                .begin_read_transaction()
                .map_err(|err| Self::storage_tx_error("BEGIN DEFERRED", &err))?;
            gate
        } else {
            // Take the writer registration before the snapshot gate so a writer already committing cannot deadlock with this snapshot.
            self.acquire_backend_writer_lock(0)?;
            let gate = match self
                .row_locks
                .begin_change_snapshot(&self.runtime.cancellation)
            {
                Ok(gate) => gate,
                Err(error) => {
                    self.row_locks.release_session(self.session_id);
                    return Err(error);
                }
            };
            if let Err(err) = backend.begin_transaction() {
                self.row_locks.release_session(self.session_id);
                return Err(Self::storage_tx_error("BEGIN", &err));
            }
            gate
        };
        if let Err(error) = self.refresh_pinned_transaction_snapshot() {
            let refresh_error = Self::storage_tx_error("BEGIN pinned snapshot refresh", &error);
            let recovered = self.recover_failed_begin_refresh(
                backend.as_ref(),
                session_snapshot,
                refresh_error,
            );
            self.row_locks.release_session(self.session_id);
            return Err(recovered);
        }
        let baseline = match snapshot_gate.baseline() {
            Ok(baseline) => baseline,
            Err(error) => {
                let recovered =
                    self.recover_failed_begin_refresh(backend.as_ref(), session_snapshot, error);
                self.row_locks.release_session(self.session_id);
                return Err(recovered);
            }
        };
        Ok((self.snapshot_transaction_data()?, baseline))
    }

    /// Register the physical backend writer this session holds in the logical lock manager. Eager writer frames (typed `begin`, direct transactions) and promoted deferred frames must both hold the structural backend-writer lock, otherwise a writer blocked on a row lock is invisible to the deadlock detector and a promoting SQL session waits at the storage layer instead of reporting `40P01`.
    pub(super) fn acquire_backend_writer_lock(&self, mark: u32) -> Result<(), SQLError> {
        self.row_locks.acquire_relation(
            self.session_id,
            self.row_locks.backend_writer_key(),
            crate::row_locks::RelationLockMode::AccessExclusive,
            mark,
            &self.runtime.cancellation,
        )
    }

    fn recover_failed_begin_refresh(
        &self,
        backend: &dyn uqa_storage::PersistentStorageBackend,
        session_snapshot: &SessionStateSnapshot,
        refresh_error: SQLError,
    ) -> SQLError {
        let mut cleanup_errors = Vec::new();
        if let Err(error) = backend.rollback_transaction() {
            cleanup_errors.push(format!("storage rollback: {error}"));
        } else {
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
        self.restore_session_state(session_snapshot);
        if cleanup_errors.is_empty() {
            refresh_error
        } else {
            SQLError::Internal(format!(
                "{refresh_error}; failed BEGIN refresh cleanup: {}",
                cleanup_errors.join("; ")
            ))
        }
    }
    pub(super) fn storage_tx_error(action: &str, err: &StorageBackendError) -> SQLError {
        SQLError::Internal(format!("{action} failed in storage backend: {err}"))
    }
}
