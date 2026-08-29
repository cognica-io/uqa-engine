//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeSet, BackendTransactionMode, ConstraintModeState, Engine, EngineDataSnapshot,
    FixedTransactionSnapshot, NontransactionalColumnStats, SQLError, SQLParam, SQLResult,
    SessionStateSnapshot, StorageBackendError, StorageBackendResult, StorageSavepointId,
    TransactionCharacteristicsState, TransactionDirtyState, TransactionFrame, TransactionIntent,
    TransactionRelationStates, TransactionRowChange, TransactionSavepoint, TransactionStatus,
};

mod characteristics;
mod control;
mod snapshots;

fn panic_description(payload: &(dyn std::any::Any + Send)) -> &str {
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
        constraint_modes: ConstraintModeState::default(),
        intent: frame.intent,
        characteristics: frame.characteristics,
        first_snapshot_set: frame.first_snapshot_set,
    }
}

impl Engine {
    fn transaction_dirty_state(&self) -> TransactionDirtyState {
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

    fn transaction_relation_states(&self) -> TransactionRelationStates {
        self.storage
            .tables
            .read()
            .iter()
            .map(|(relation, table)| (relation.clone(), table.lifecycle_id()))
            .collect()
    }

    fn restore_transaction_dirty_state(&self, state: TransactionDirtyState) {
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
            false,
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

    /// Run `f` inside one engine transaction. On success the transaction is
    /// committed; on error or panic it is rolled back before the error/panic is
    /// returned to the caller.
    pub fn transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        self.begin()?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                self.commit()?;
                Ok(value)
            }
            Ok(Err(err)) => {
                if let Err(rollback_err) = self.rollback() {
                    return Err(SQLError::Internal(format!(
                        "transaction rollback after error failed: {rollback_err}; original error: {err}"
                    )));
                }
                Err(err)
            }
            Err(payload) => match self.rollback() {
                Ok(()) => std::panic::resume_unwind(payload),
                Err(rollback_err) => Err(SQLError::Internal(format!(
                    "transaction rollback after panic failed: {rollback_err}; original panic: {}",
                    panic_description(payload.as_ref())
                ))),
            },
        }
    }

    /// Make a direct persistent-engine mutation atomic when the caller has not
    /// already opened a transaction. Memory stores validate fallible vector
    /// input before their infallible writes; explicit memory transactions use
    /// deep writable snapshots. Avoiding a whole-engine snapshot for each
    /// direct memory insert keeps bulk ingestion linear.
    pub(crate) fn with_implicit_transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        if self.current_transaction_is_read_only() {
            return Err(SQLError::Routine {
                sqlstate: "25006".into(),
                message: "cannot execute direct mutation in a read-only transaction".into(),
            });
        }
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()?;
            self.prepare_explicit_transaction_writer()?;
            return f(self);
        }
        if self.storage.backend.is_none() {
            return f(self);
        }
        self.transaction(|engine| {
            engine.prepare_explicit_transaction_writer()?;
            f(engine)
        })
    }

    /// Error-type-preserving counterpart for direct APIs whose public error
    /// type is not [`SQLError`]. `map_transaction_error` is used only for
    /// begin/commit/rollback infrastructure failures; an error returned by
    /// `f` is passed through unchanged when rollback succeeds.
    pub(crate) fn with_implicit_mapped_transaction<R, E>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, E>,
        map_transaction_error: impl Fn(String) -> E,
    ) -> Result<R, E>
    where
        E: std::fmt::Display,
    {
        let _statement = self.runtime.statement_gate.lock();
        if self.current_transaction_is_read_only() {
            return Err(map_transaction_error(
                "cannot execute direct mutation in a read-only transaction".into(),
            ));
        }
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()
                .map_err(|error| map_transaction_error(error.to_string()))?;
            self.prepare_explicit_transaction_writer()
                .map_err(|error| {
                    map_transaction_error(format!(
                        "promote explicit engine transaction failed: {error}"
                    ))
                })?;
            return f(self);
        }
        if self.storage.backend.is_none() {
            return f(self);
        }
        self.begin().map_err(|error| {
            map_transaction_error(format!("begin implicit engine transaction failed: {error}"))
        })?;
        if let Err(error) = self.prepare_explicit_transaction_writer() {
            let error = map_transaction_error(format!(
                "promote implicit engine transaction failed: {error}"
            ));
            return match self.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(map_transaction_error(format!(
                    "rollback implicit engine transaction after promotion failure failed: {rollback_error}; original error: {error}"
                ))),
            };
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                self.commit().map_err(|error| {
                    map_transaction_error(format!(
                        "commit implicit engine transaction failed: {error}"
                    ))
                })?;
                Ok(value)
            }
            Ok(Err(error)) => match self.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(map_transaction_error(format!(
                    "rollback implicit engine transaction failed: {rollback_error}; original error: {error}"
                ))),
            },
            Err(payload) => match self.rollback() {
                Ok(()) => std::panic::resume_unwind(payload),
                Err(rollback_error) => Err(map_transaction_error(format!(
                    "rollback implicit engine transaction after panic failed: {rollback_error}; original panic: {}",
                    panic_description(payload.as_ref())
                ))),
            },
        }
    }

    /// Storage-facing counterpart of [`Engine::with_implicit_transaction`].
    /// The storage error is retained verbatim when rollback succeeds so API
    /// callers can still classify the original backend failure.
    pub(crate) fn with_implicit_storage_transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        if self.current_transaction_is_read_only() {
            return Err(StorageBackendError::Other(
                "cannot execute storage mutation in a read-only transaction".into(),
            ));
        }
        self.with_implicit_storage_transaction_inner(false, f)
    }

    /// Run storage maintenance that `PostgreSQL` permits in a read-only transaction. The transaction remains logically read-only, while its physical backend is allowed to persist maintenance metadata such as ANALYZE statistics.
    pub(crate) fn with_read_only_compatible_storage_transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        if self.transaction_depth() != 0 && self.current_transaction_is_read_only() {
            self.ensure_transaction_usable()
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            return f(self);
        }
        self.with_implicit_storage_transaction_inner(true, f)
    }

    fn with_implicit_storage_transaction_inner<R>(
        &self,
        maintenance_can_override_default_read_only: bool,
        f: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        let _statement = self.runtime.statement_gate.lock();
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            self.prepare_explicit_transaction_writer()
                .map_err(|error| {
                    StorageBackendError::Other(format!(
                        "promote explicit engine transaction failed: {error}"
                    ))
                })?;
            return f(self);
        }
        self.begin().map_err(|error| {
            StorageBackendError::Other(format!("begin implicit engine transaction failed: {error}"))
        })?;
        if maintenance_can_override_default_read_only {
            if let Some(frame) = self.session.transactions.lock().last_mut() {
                frame.intent = TransactionIntent::ReadWrite;
                frame.characteristics.read_only = false;
            }
        }
        if let Err(error) = self.prepare_explicit_transaction_writer() {
            let error = StorageBackendError::Other(format!(
                "promote implicit engine transaction failed: {error}"
            ));
            return match self.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(StorageBackendError::Other(format!(
                    "rollback implicit engine transaction after promotion failure failed: {rollback_error}; original error: {error}"
                ))),
            };
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                self.commit().map_err(|error| {
                    StorageBackendError::Other(format!(
                        "commit implicit engine transaction failed: {error}"
                    ))
                })?;
                Ok(value)
            }
            Ok(Err(error)) => match self.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(StorageBackendError::Other(format!(
                    "rollback implicit engine transaction failed: {rollback_error}; original error: {error}"
                ))),
            },
            Err(payload) => match self.rollback() {
                Ok(()) => std::panic::resume_unwind(payload),
                Err(rollback_error) => Err(StorageBackendError::Other(format!(
                    "rollback implicit engine transaction after panic failed: {rollback_error}; original panic: {}",
                    panic_description(payload.as_ref())
                ))),
            },
        }
    }

    pub(crate) fn with_implicit_string_transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, String>,
    ) -> Result<R, String> {
        let _statement = self.runtime.statement_gate.lock();
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()
                .map_err(|error| error.to_string())?;
            self.prepare_explicit_transaction_writer()
                .map_err(|error| format!("promote explicit engine transaction failed: {error}"))?;
            return f(self);
        }
        self.begin()
            .map_err(|error| format!("begin implicit engine transaction failed: {error}"))?;
        if let Err(error) = self.prepare_explicit_transaction_writer() {
            let error = format!("promote implicit engine transaction failed: {error}");
            return match self.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "rollback implicit engine transaction after promotion failure failed: {rollback_error}; original error: {error}"
                )),
            };
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                self.commit()
                    .map_err(|error| format!("commit implicit engine transaction failed: {error}"))?;
                Ok(value)
            }
            Ok(Err(error)) => match self.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "rollback implicit engine transaction failed: {rollback_error}; original error: {error}"
                )),
            },
            Err(payload) => match self.rollback() {
                Ok(()) => std::panic::resume_unwind(payload),
                Err(rollback_error) => Err(format!(
                    "rollback implicit engine transaction after panic failed: {rollback_error}; original panic: {}",
                    panic_description(payload.as_ref())
                )),
            },
        }
    }

    /// Execute multiple SQL statements inside one engine transaction.
    pub fn sql_batch(
        &self,
        statements: &[(&str, &[SQLParam])],
    ) -> Result<Vec<SQLResult>, SQLError> {
        self.transaction(|engine| {
            let mut results = Vec::with_capacity(statements.len());
            for (sql, params) in statements {
                results.push(engine.sql(sql, params)?);
            }
            Ok(results)
        })
    }

    /// Number of currently-open transaction frames (`BEGIN` count
    /// minus `COMMIT/ROLLBACK` count). Useful for assertions in tests
    /// and for status displays in the CLI.
    pub fn transaction_depth(&self) -> usize {
        self.session.transactions.lock().len()
    }

    pub(crate) fn in_transaction_block(&self) -> bool {
        self.session
            .transactions
            .lock()
            .last()
            .is_some_and(|frame| !frame.implicit_statement)
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
            true,
            characteristics,
        )
    }

    pub(crate) fn begin_implicit_transaction_block(&self) -> Result<(), SQLError> {
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
            false,
            characteristics,
        )
    }

    pub(crate) fn promote_implicit_transaction_block(&self) -> Result<(), SQLError> {
        let mut stack = self.session.transactions.lock();
        let frame = stack.last_mut().ok_or_else(|| {
            SQLError::Internal("promote implicit transaction without an open frame".into())
        })?;
        frame.implicit_statement = false;
        Ok(())
    }

    fn begin_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
        read_only: bool,
        defer_write_lock: bool,
        implicit_statement: bool,
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
        let (constraint_modes, deferred_foreign_key_checks) = stack.last().map_or_else(
            || (ConstraintModeState::default(), Vec::new()),
            |frame| {
                (
                    frame.constraint_modes.clone(),
                    frame.deferred_foreign_key_checks.clone(),
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
        stack.push(TransactionFrame {
            implicit_statement,
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
            constraint_modes,
            nontransactional_column_stats: NontransactionalColumnStats::new(),
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
    fn acquire_backend_writer_lock(&self, mark: u32) -> Result<(), SQLError> {
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

    fn commit_transaction_frame(
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
            parent.first_snapshot_set |= committed.first_snapshot_set;
        }
        Ok(())
    }

    /// A failed outer backend COMMIT/ROLLBACK has already ended the managed
    /// storage transaction; a failed nested savepoint finish aborts the
    /// enclosing transaction explicitly. In every case the engine stack and
    /// session-local caches are restored before the error escapes, so callers
    /// never inherit a ghost transaction or uncommitted catalog state.
    fn recover_failed_transaction_finish(
        &self,
        stack: &mut Vec<TransactionFrame>,
        nested: bool,
        finish_error: SQLError,
    ) -> SQLError {
        let raw_nontransactional_column_stats = stack
            .first()
            .map(|frame| frame.nontransactional_column_stats.clone())
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
        if let Some(snapshot) = session_snapshot.as_ref() {
            self.restore_session_state(snapshot);
        }
        if cleanup_errors.is_empty() {
            finish_error
        } else {
            SQLError::Internal(format!(
                "{finish_error}; failed transaction cleanup: {}",
                cleanup_errors.join("; ")
            ))
        }
    }

    fn retain_nontransactional_stats_for_rollback(
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

    fn rollback_backend_transaction_frame(
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

    fn rollback_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
    ) -> Result<(), SQLError> {
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
        self.restore_session_state(&session_snapshot);
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

    fn persist_nontransactional_column_stats_after_rollback(
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

    fn apply_nontransactional_column_stats(
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

    fn nontransactional_column_stats_after_rollback(
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
        let raw_nontransactional_column_stats = stack
            .first()
            .map(|frame| frame.nontransactional_column_stats.clone())
            .unwrap_or_default();
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
        self.restore_session_state(&rollback_state.session);
        self.release_aborted_statement_locks(rollback_state.keep_mark);
        if let Some(frame) = stack.last_mut() {
            frame.status = if backend_aborted {
                TransactionStatus::FailedBackendAborted
            } else {
                TransactionStatus::Failed
            };
            frame.row_changes = rollback_state.row_changes;
            frame.deferred_foreign_key_checks = rollback_state.deferred_foreign_key_checks;
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

    /// A deferred outer frame still runs a backend read transaction, which cannot carry backend savepoints. Savepoints are then recorded on the frame only; promotion to a writer recreates every recorded savepoint on the write transaction, so `PostgreSQL`'s fresh READ COMMITTED snapshot per statement survives `SAVEPOINT` and nested `BEGIN`.
    fn backend_savepoints_deferred(stack: &[TransactionFrame]) -> bool {
        stack
            .first()
            .is_some_and(|frame| frame.backend_mode == BackendTransactionMode::Deferred)
    }

    fn save_transaction_savepoint(
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
            constraint_modes,
        });
        frame.xid_levels.push(None);
        Ok(())
    }

    fn release_transaction_savepoint(
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

    fn rollback_to_transaction_savepoint(
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
        self.restore_session_state(&savepoint.session_snapshot);
        let keep_mark = savepoint.lock_mark;
        frame.row_changes.clone_from(&savepoint.row_changes);
        frame
            .deferred_foreign_key_checks
            .clone_from(&savepoint.deferred_foreign_key_checks);
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

    fn storage_tx_error(action: &str, err: &StorageBackendError) -> SQLError {
        SQLError::Internal(format!("{action} failed in storage backend: {err}"))
    }

    pub(crate) fn current_lock_mark(&self) -> u32 {
        self.session
            .transactions
            .lock()
            .last()
            .map_or(0, |frame| frame.lock_mark)
    }

    /// Select the storage snapshot for one explicit SQL statement. READ COMMITTED refreshes an unwritten deferred transaction per statement. REPEATABLE READ and SERIALIZABLE pin an independent read session at the first snapshot-bearing statement so later writer promotion cannot discard the fixed view.
    pub(crate) fn prepare_explicit_statement_snapshot(
        &self,
        sets_transaction_snapshot: bool,
    ) -> Result<(), SQLError> {
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(());
        };
        // A statement issued by a host callback while an outer statement is still executing must keep the outer statement's snapshot: replacing the backend read transaction underneath a running scan would mix snapshots or abort the outer cursor. Only the outermost statement of the session takes a fresh READ COMMITTED snapshot.
        if self.session.row_lock_statements.lock().len() > 1 {
            return Ok(());
        }
        let _statement = self.runtime.statement_gate.lock();
        let mut stack = self.session.transactions.lock();
        let fixed_snapshot_already_set = stack
            .first()
            .is_some_and(|frame| frame.fixed_snapshot.is_some());
        if fixed_snapshot_already_set {
            return Ok(());
        }
        if !stack
            .first()
            .is_some_and(|frame| frame.backend_mode == BackendTransactionMode::Deferred)
        {
            return Ok(());
        }
        if backend
            .transaction_has_written()
            .map_err(|error| Self::storage_tx_error("inspect statement snapshot", &error))?
        {
            return Ok(());
        }
        let snapshot_gate = self
            .row_locks
            .begin_change_snapshot(&self.runtime.cancellation)?;
        let establish_fixed_snapshot = sets_transaction_snapshot
            && stack.first().is_some_and(|frame| {
                matches!(
                    frame.characteristics.isolation,
                    uqa_sql::ast::TransactionIsolationLevel::RepeatableRead
                        | uqa_sql::ast::TransactionIsolationLevel::Serializable
                )
            });
        self.replace_unwritten_backend_transaction(&mut stack, true, "refresh statement snapshot")?;
        if establish_fixed_snapshot {
            let catalog_baseline = self.capture_fixed_transaction_catalog_baseline()?;
            let snapshot = if backend.supports_concurrent_pinned_read_and_write() {
                FixedTransactionSnapshot::Pinned(self.open_independent_pinned_read_snapshot()?)
            } else {
                let snapshot = self.capture_detached_fixed_transaction_snapshot()?;
                self.restart_backend_after_detached_snapshot(&mut stack)?;
                FixedTransactionSnapshot::Detached(snapshot)
            };
            let frame = stack.first_mut().ok_or_else(|| {
                SQLError::Internal("fixed snapshot transaction frame disappeared".into())
            })?;
            frame.fixed_snapshot = Some(snapshot);
            frame.fixed_catalog_baseline = Some(catalog_baseline);
        }
        let baseline = match snapshot_gate.baseline() {
            Ok(baseline) => baseline,
            Err(error) => {
                return Err(self.abort_failed_backend_transaction_replacement(
                    &mut stack,
                    backend.as_ref(),
                    error,
                ));
            }
        };
        stack[0].snapshot_change_baseline = baseline;
        self.update_statement_row_lock_baseline(baseline);
        Ok(())
    }

    /// A detached fixed snapshot no longer needs the backend read transaction that produced it. Restart a bare deferred transaction without pinning or reloading caches so rollback-journal backends release their read lock while the logical transaction stays open.
    fn restart_backend_after_detached_snapshot(
        &self,
        stack: &mut Vec<TransactionFrame>,
    ) -> Result<(), SQLError> {
        let backend = self.storage.backend.as_ref().ok_or_else(|| {
            SQLError::Internal("detached fixed snapshot requires persistent storage".into())
        })?;
        if let Err(error) = backend.rollback_transaction() {
            let failure = Self::storage_tx_error("release detached fixed snapshot reader", &error);
            return Err(self.abort_failed_backend_transaction_replacement(
                stack,
                backend.as_ref(),
                failure,
            ));
        }
        if let Err(error) = backend.begin_read_transaction() {
            let failure =
                Self::storage_tx_error("restart detached fixed snapshot transaction", &error);
            return Err(self.abort_failed_backend_transaction_replacement(
                stack,
                backend.as_ref(),
                failure,
            ));
        }
        stack[0].backend_mode = BackendTransactionMode::Deferred;
        Ok(())
    }

    /// Release an unwritten backend reader before an autonomous maintenance write. The replacement is a bare deferred transaction, so the SQL transaction remains open without retaining a rollback-journal read lock.
    pub(crate) fn release_backend_reader_for_independent_maintenance(
        &self,
    ) -> Result<(), SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let mut stack = self.session.transactions.lock();
        let backend = self.storage.backend.as_ref().ok_or_else(|| {
            SQLError::Internal("independent maintenance requires persistent storage".into())
        })?;
        if backend
            .transaction_has_written()
            .map_err(|error| Self::storage_tx_error("inspect maintenance reader", &error))?
        {
            return Err(SQLError::Internal(
                "cannot release a backend transaction that already contains writes".into(),
            ));
        }
        self.restart_backend_after_detached_snapshot(&mut stack)
    }

    pub(crate) fn open_independent_pinned_read_snapshot(&self) -> Result<Box<Engine>, SQLError> {
        let snapshot = self.new_session().map_err(|error| {
            SQLError::Internal(format!("open fixed transaction snapshot session: {error}"))
        })?;
        let backend = snapshot.storage.backend.as_ref().ok_or_else(|| {
            SQLError::Internal("fixed transaction snapshot requires persistent storage".into())
        })?;
        backend.begin_read_transaction().map_err(|error| {
            SQLError::Internal(format!("begin fixed transaction snapshot: {error}"))
        })?;
        if let Err(error) = snapshot.refresh_pinned_transaction_snapshot() {
            let rollback = backend.rollback_transaction();
            return Err(match rollback {
                Ok(()) => SQLError::Internal(format!("pin fixed transaction snapshot: {error}")),
                Err(rollback_error) => SQLError::Internal(format!(
                    "pin fixed transaction snapshot: {error}; rollback also failed: {rollback_error}"
                )),
            });
        }
        let lifetimes = self
            .storage
            .tables
            .read()
            .iter()
            .map(|(relation, table)| {
                (
                    relation.clone(),
                    (table.lifecycle_id(), table.storage_generation()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        for (relation, table) in snapshot.storage.tables.read().iter() {
            if let Some((lifecycle_id, storage_generation)) = lifetimes.get(relation) {
                if *storage_generation == table.storage_generation() {
                    table
                        .lifecycle_id
                        .store(*lifecycle_id, std::sync::atomic::Ordering::Release);
                }
            }
        }
        Ok(Box::new(snapshot))
    }

    pub(crate) fn refresh_explicit_statement_snapshot(&self) -> Result<(), SQLError> {
        self.prepare_explicit_statement_snapshot(false)
    }

    pub(crate) fn prepare_explicit_transaction_writer(&self) -> Result<bool, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let mut stack = self.session.transactions.lock();
        if !stack
            .first()
            .is_some_and(|frame| frame.backend_mode == BackendTransactionMode::Deferred)
        {
            return Ok(false);
        }
        self.promote_deferred_transaction_frame(&mut stack)?;
        Ok(true)
    }

    pub(crate) fn backend_transaction_is_deferred(&self) -> bool {
        self.session
            .transactions
            .lock()
            .first()
            .is_some_and(|frame| frame.backend_mode == BackendTransactionMode::Deferred)
    }

    fn promote_deferred_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
    ) -> Result<(), SQLError> {
        if !stack
            .first()
            .is_some_and(|frame| frame.backend_mode == BackendTransactionMode::Deferred)
        {
            return Ok(());
        }
        // The physical writer lives until the outer transaction ends, so its logical registration must survive ROLLBACK TO SAVEPOINT and an error rollback that releases the current savepoint's lock mark.
        let mark = stack.first().map_or(0, |frame| frame.begin_lock_mark);
        self.acquire_backend_writer_lock(mark)?;
        if self.storage.backend.is_none() {
            if let Some(frame) = stack.first_mut() {
                frame.backend_mode = BackendTransactionMode::Writer;
            }
            return Ok(());
        }
        self.replace_unwritten_backend_transaction(
            stack,
            false,
            "promote explicit transaction to writer",
        )
    }

    fn replace_unwritten_backend_transaction(
        &self,
        stack: &mut Vec<TransactionFrame>,
        deferred: bool,
        action: &str,
    ) -> Result<(), SQLError> {
        let Some(backend) = self.storage.backend.as_ref() else {
            return Err(SQLError::Internal(format!(
                "{action} requires persistent storage"
            )));
        };
        let written = backend
            .transaction_has_written()
            .map_err(|error| Self::storage_tx_error(&format!("inspect {action}"), &error))?;
        if stack.is_empty() || written {
            return Err(SQLError::Internal(format!(
                "{action} requires an open transaction without storage writes"
            )));
        }
        if let Err(error) = backend.rollback_transaction() {
            let failure = Self::storage_tx_error(action, &error);
            return Err(self.abort_failed_backend_transaction_replacement(
                stack,
                backend.as_ref(),
                failure,
            ));
        }
        let begin = if deferred {
            backend.begin_read_transaction()
        } else {
            backend.begin_transaction()
        };
        if let Err(error) = begin {
            let failure = Self::storage_tx_error(action, &error);
            return Err(self.abort_failed_backend_transaction_replacement(
                stack,
                backend.as_ref(),
                failure,
            ));
        }
        // Writer promotion materializes every logical savepoint in creation order: each frame's own nested-BEGIN savepoint precedes the user savepoints declared inside that frame, and inner frames follow their parents. A refreshed read transaction keeps them logical.
        let mut storage_savepoints = Vec::new();
        if !deferred {
            for frame in stack.iter() {
                if let Some(savepoint) = frame.storage_savepoint {
                    storage_savepoints.push(savepoint);
                }
                storage_savepoints.extend(
                    frame
                        .savepoints
                        .iter()
                        .map(|savepoint| savepoint.storage_savepoint),
                );
            }
        }
        for storage_savepoint in storage_savepoints {
            if let Err(error) = backend.savepoint(storage_savepoint) {
                let failure = SQLError::Internal(format!(
                    "recreate storage savepoint after {action} failed: {error}"
                ));
                return Err(self.abort_failed_backend_transaction_replacement(
                    stack,
                    backend.as_ref(),
                    failure,
                ));
            }
        }
        if let Err(error) = self.refresh_pinned_transaction_snapshot() {
            let failure = SQLError::Internal(format!("refresh after {action} failed: {error}"));
            return Err(self.abort_failed_backend_transaction_replacement(
                stack,
                backend.as_ref(),
                failure,
            ));
        }
        stack[0].backend_mode = if deferred {
            BackendTransactionMode::Deferred
        } else {
            BackendTransactionMode::Writer
        };
        Ok(())
    }

    fn abort_failed_backend_transaction_replacement(
        &self,
        stack: &mut Vec<TransactionFrame>,
        backend: &dyn uqa_storage::PersistentStorageBackend,
        failure: SQLError,
    ) -> SQLError {
        let failure = if backend.in_transaction() {
            match backend.rollback_transaction() {
                Ok(()) => failure,
                Err(rollback_error) => SQLError::Internal(format!(
                    "{failure}; replacement rollback also failed: {rollback_error}"
                )),
            }
        } else {
            failure
        };
        self.recover_failed_transaction_finish(stack, false, failure)
    }

    fn snapshot_session_state(&self) -> SessionStateSnapshot {
        let mut snapshot = self.session.state.read().clone();
        snapshot.portal_names = self.session.portals.lock().keys().cloned().collect();
        snapshot
    }

    fn restore_session_state(&self, snapshot: &SessionStateSnapshot) {
        *self.session.state.write() = snapshot.clone();
        self.session
            .portals
            .lock()
            .retain(|name, _| snapshot.portal_names.contains(name));
    }
}

fn failed_transaction_error() -> SQLError {
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

mod completion;
mod constraints;
pub(crate) use constraints::constraint_identities_match;
mod row_locks_session;

#[cfg(test)]
mod tests;
