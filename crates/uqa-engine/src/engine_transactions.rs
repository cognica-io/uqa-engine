//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, Engine, EngineDataSnapshot, SQLError, SQLParam, SQLResult, SessionStateSnapshot,
    StorageBackendError, StorageBackendResult, TableDataSnapshot, TransactionDirtyState,
    TransactionFrame,
};
use uqa_sql::ast::TransactionStmt;

fn panic_description(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

impl Engine {
    fn transaction_dirty_state(&self) -> TransactionDirtyState {
        TransactionDirtyState {
            table_data: self
                .table_data_dirty
                .load(std::sync::atomic::Ordering::Acquire),
            table_catalog: self
                .table_catalog_dirty
                .load(std::sync::atomic::Ordering::Acquire),
            catalog_registry: self
                .catalog_registry_dirty
                .load(std::sync::atomic::Ordering::Acquire),
        }
    }

    fn restore_transaction_dirty_state(&self, state: TransactionDirtyState) {
        self.table_data_dirty
            .store(state.table_data, std::sync::atomic::Ordering::Release);
        self.table_catalog_dirty
            .store(state.table_catalog, std::sync::atomic::Ordering::Release);
        self.catalog_registry_dirty
            .store(state.catalog_registry, std::sync::atomic::Ordering::Release);
    }

    pub fn begin(&self) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Begin)
    }

    /// Commit the top-most transaction frame. Matches UQA behavior for
    /// `Engine.commit`.
    pub fn commit(&self) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit)
    }

    /// Roll back the top-most transaction frame. Matches UQA behavior for
    /// `Engine.rollback`.
    pub fn rollback(&self) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Rollback)
    }

    /// Mark a savepoint inside the current transaction. Matches UQA behavior for
    /// `Engine.savepoint(name)`.
    pub fn savepoint(&self, name: &str) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Savepoint(name.to_string()))
    }

    /// Release a savepoint. Matches UQA behavior for `Engine.release_savepoint`.
    pub fn release_savepoint(&self, name: &str) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::ReleaseSavepoint(
            name.to_string(),
        ))
    }

    /// Roll back to a named savepoint. Matches UQA behavior for
    /// `Engine.rollback_to_savepoint`.
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
        let _statement = self.statement_gate.lock();
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
        let _statement = self.statement_gate.lock();
        if self.transaction_depth() != 0 || self.backend.is_none() {
            return f(self);
        }
        self.transaction(f)
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
        let _statement = self.statement_gate.lock();
        if self.transaction_depth() != 0 || self.backend.is_none() {
            return f(self);
        }
        self.begin().map_err(|error| {
            map_transaction_error(format!("begin implicit engine transaction failed: {error}"))
        })?;
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
        let _statement = self.statement_gate.lock();
        if self.transaction_depth() != 0 {
            return f(self);
        }
        self.begin().map_err(|error| {
            StorageBackendError::Other(format!("begin implicit engine transaction failed: {error}"))
        })?;
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
        let _statement = self.statement_gate.lock();
        if self.transaction_depth() != 0 {
            return f(self);
        }
        self.begin()
            .map_err(|error| format!("begin implicit engine transaction failed: {error}"))?;
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
        self.tx_stack.lock().len()
    }

    /// Tear down engine state cleanly. Rolls back any open transaction
    /// frames and clears registries. Matches UQA behavior for `Engine.close`.
    /// The engine value can no longer be used afterwards in a
    /// well-defined sense; idiomatic Rust drops the value at scope
    /// exit, but this method exists for API compatibility
    /// reference and for explicit shutdown ordering.
    pub fn close(&self) -> Result<(), SQLError> {
        let _statement = self.statement_gate.lock();
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

    pub fn run_transaction_statement(&self, tx: TransactionStmt) -> Result<(), SQLError> {
        let _statement = self.statement_gate.lock();
        let mut guard = self.tx_stack.lock();
        match tx {
            TransactionStmt::Begin => self.begin_transaction_frame(&mut guard, false)?,
            TransactionStmt::Commit => self.commit_transaction_frame(&mut guard)?,
            TransactionStmt::Rollback => self.rollback_transaction_frame(&mut guard)?,
            TransactionStmt::Savepoint(name) => {
                self.save_transaction_savepoint(&mut guard, name)?;
            }
            TransactionStmt::ReleaseSavepoint(name) => {
                self.release_transaction_savepoint(&mut guard, &name)?;
            }
            TransactionStmt::RollbackToSavepoint(name) => {
                self.rollback_to_transaction_savepoint(&mut guard, &name)?;
            }
        }
        Ok(())
    }

    pub(crate) fn begin_implicit_statement_transaction(
        &self,
        read_only: bool,
    ) -> Result<(), SQLError> {
        let _statement = self.statement_gate.lock();
        let mut stack = self.tx_stack.lock();
        if !stack.is_empty() {
            return Err(SQLError::Internal(
                "implicit statement transaction started inside an explicit transaction".into(),
            ));
        }
        self.begin_transaction_frame(&mut stack, read_only)
    }

    fn begin_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
        read_only: bool,
    ) -> Result<(), SQLError> {
        let session_snapshot = self.snapshot_session_state();
        let (storage_savepoint, data_snapshot) = if stack.is_empty() {
            if let Some(backend) = self.backend.as_ref() {
                if read_only {
                    backend
                        .begin_read_transaction()
                        .map_err(|err| Self::storage_tx_error("BEGIN DEFERRED", &err))?;
                } else {
                    backend
                        .begin_transaction()
                        .map_err(|err| Self::storage_tx_error("BEGIN", &err))?;
                }
                if let Err(error) = self.refresh_pinned_transaction_snapshot() {
                    let refresh_error =
                        Self::storage_tx_error("BEGIN pinned snapshot refresh", &error);
                    return Err(self.recover_failed_begin_refresh(
                        backend.as_ref(),
                        &session_snapshot,
                        refresh_error,
                    ));
                }
                (None, None)
            } else {
                self.synchronize_table_catalog()
                    .map_err(|err| Self::storage_tx_error("BEGIN table catalog refresh", &err))?;
                self.synchronize_table_data()
                    .map_err(|err| Self::storage_tx_error("BEGIN table data refresh", &err))?;
                self.synchronize_catalog_registries()
                    .map_err(|err| Self::storage_tx_error("BEGIN registry refresh", &err))?;
                (None, self.snapshot_transaction_data()?)
            }
        } else {
            let savepoint = format!("__uqa_nested_tx_{}", stack.len());
            if let Some(backend) = self.backend.as_ref() {
                backend
                    .savepoint(&savepoint)
                    .map_err(|err| Self::storage_tx_error("nested BEGIN savepoint", &err))?;
                (Some(savepoint), None)
            } else {
                (Some(savepoint), self.snapshot_transaction_data()?)
            }
        };
        stack.push(TransactionFrame {
            storage_savepoint,
            read_only,
            savepoints: std::collections::BTreeSet::new(),
            session_snapshot,
            session_savepoints: BTreeMap::new(),
            data_snapshot,
            data_savepoints: BTreeMap::new(),
            dirty_at_begin: self.transaction_dirty_state(),
            dirty_savepoints: BTreeMap::new(),
        });
        Ok(())
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

    fn commit_transaction_frame(&self, stack: &mut Vec<TransactionFrame>) -> Result<(), SQLError> {
        let frame = stack
            .last()
            .ok_or_else(|| SQLError::Internal("COMMIT without an open transaction".into()))?;
        let storage_savepoint = frame.storage_savepoint.clone();
        let read_only = frame.read_only;
        if read_only && storage_savepoint.is_none() {
            if let Some(connection) = self.sqlite_session.as_ref() {
                let violation = match connection.transaction_has_written() {
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
        if let Some(backend) = self.backend.as_ref() {
            let commit_result = if let Some(savepoint) = storage_savepoint.as_ref() {
                backend
                    .release_savepoint(savepoint)
                    .map_err(|err| Self::storage_tx_error("nested COMMIT savepoint", &err))
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
        if storage_savepoint.is_none() {
            if self
                .table_catalog_dirty
                .load(std::sync::atomic::Ordering::Acquire)
            {
                self.publish_table_catalog_changes();
            }
            if self
                .catalog_registry_dirty
                .load(std::sync::atomic::Ordering::Acquire)
            {
                self.publish_catalog_registry_changes();
            }
            if self
                .table_data_dirty
                .load(std::sync::atomic::Ordering::Acquire)
            {
                self.publish_table_data_changes();
            }
        }
        stack.pop();
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
        let session_snapshot = stack.first().map(|frame| frame.session_snapshot.clone());
        let snapshot = stack.first().and_then(|frame| frame.data_snapshot.clone());
        let dirty_at_begin = stack
            .first()
            .map_or_else(TransactionDirtyState::default, |frame| frame.dirty_at_begin);
        let mut cleanup_errors = Vec::new();
        if nested {
            if let Some(backend) = self.backend.as_ref() {
                if let Err(error) = backend.rollback_transaction() {
                    cleanup_errors.push(format!("storage rollback: {error}"));
                }
            }
        }
        stack.clear();
        self.restore_transaction_dirty_state(dirty_at_begin);
        if let Some(snapshot) = snapshot.as_ref() {
            if let Err(error) = self.restore_transaction_data(snapshot) {
                cleanup_errors.push(format!("memory restore: {error}"));
            }
        }
        if self.backend.is_some() {
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

    fn rollback_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
    ) -> Result<(), SQLError> {
        let storage_savepoint = stack
            .last()
            .ok_or_else(|| SQLError::Internal("ROLLBACK without an open transaction".into()))?
            .storage_savepoint
            .clone();
        if let Some(backend) = self.backend.as_ref() {
            let rollback_result = if let Some(savepoint) = storage_savepoint.as_ref() {
                backend
                    .rollback_to_savepoint(savepoint)
                    .map_err(|err| Self::storage_tx_error("nested ROLLBACK savepoint", &err))
                    .and_then(|()| {
                        backend
                            .release_savepoint(savepoint)
                            .map_err(|err| Self::storage_tx_error("nested ROLLBACK release", &err))
                    })
            } else {
                backend
                    .rollback_transaction()
                    .map_err(|err| Self::storage_tx_error("ROLLBACK", &err))
            };
            if let Err(rollback_error) = rollback_result {
                return Err(self.recover_failed_transaction_finish(
                    stack,
                    storage_savepoint.is_some(),
                    rollback_error,
                ));
            }
        }
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
        if let Err(error) = self.reload_persistent_value_indexes() {
            cleanup_errors.push(format!("btree restore: {error}"));
        }
        if self.backend.is_some() {
            if let Err(error) = self.reload_table_catalog_after_rollback() {
                cleanup_errors.push(format!("table catalog restore: {error}"));
            }
            if let Err(error) = self.reload_catalog_registries_after_rollback() {
                cleanup_errors.push(format!("registry restore: {error}"));
            }
        }
        self.restore_session_state(&session_snapshot);
        stack.pop();
        if cleanup_errors.is_empty() {
            Ok(())
        } else {
            Err(SQLError::Internal(format!(
                "ROLLBACK completed but engine state restoration failed: {}",
                cleanup_errors.join("; ")
            )))
        }
    }

    fn save_transaction_savepoint(
        &self,
        stack: &mut [TransactionFrame],
        name: String,
    ) -> Result<(), SQLError> {
        if stack.is_empty() {
            return Err(SQLError::Internal("SAVEPOINT outside a transaction".into()));
        }
        let session_snapshot = self.snapshot_session_state();
        let data_snapshot = self.snapshot_transaction_data()?;
        let frame = stack.last_mut().ok_or_else(|| {
            SQLError::Internal("SAVEPOINT lost its checked transaction frame".into())
        })?;
        if let Some(backend) = self.backend.as_ref() {
            backend
                .savepoint(&name)
                .map_err(|err| Self::storage_tx_error("SAVEPOINT", &err))?;
        }
        if let Some(snapshot) = data_snapshot {
            frame.data_savepoints.insert(name.clone(), snapshot);
        }
        frame
            .session_savepoints
            .insert(name.clone(), session_snapshot);
        frame
            .dirty_savepoints
            .insert(name.clone(), self.transaction_dirty_state());
        frame.savepoints.insert(name);
        Ok(())
    }

    fn release_transaction_savepoint(
        &self,
        stack: &mut [TransactionFrame],
        name: &str,
    ) -> Result<(), SQLError> {
        let frame = stack
            .last_mut()
            .ok_or_else(|| SQLError::Internal("RELEASE SAVEPOINT outside a transaction".into()))?;
        if let Some(backend) = self.backend.as_ref() {
            backend
                .release_savepoint(name)
                .map_err(|err| Self::storage_tx_error("RELEASE SAVEPOINT", &err))?;
        }
        frame.savepoints.remove(name);
        frame.session_savepoints.remove(name);
        frame.data_savepoints.remove(name);
        frame.dirty_savepoints.remove(name);
        Ok(())
    }

    fn rollback_to_transaction_savepoint(
        &self,
        stack: &mut [TransactionFrame],
        name: &str,
    ) -> Result<(), SQLError> {
        let frame = stack.last_mut().ok_or_else(|| {
            SQLError::Internal("ROLLBACK TO SAVEPOINT outside a transaction".into())
        })?;
        if !frame.savepoints.contains(name) {
            return Err(SQLError::Internal(format!("savepoint `{name}` not found")));
        }
        if let Some(backend) = self.backend.as_ref() {
            backend
                .rollback_to_savepoint(name)
                .map_err(|err| Self::storage_tx_error("ROLLBACK TO SAVEPOINT", &err))?;
        }
        let mut cleanup_errors = Vec::new();
        if let Some(snapshot) = frame.data_savepoints.get(name) {
            if let Err(error) = self.restore_transaction_data(snapshot) {
                cleanup_errors.push(format!("memory restore: {error}"));
            }
        }
        if let Some(dirty) = frame.dirty_savepoints.get(name).copied() {
            self.restore_transaction_dirty_state(dirty);
        }
        if let Err(error) = self.reload_persistent_value_indexes() {
            cleanup_errors.push(format!("btree restore: {error}"));
        }
        if self.backend.is_some() {
            if let Err(error) = self.reload_table_catalog_after_rollback() {
                cleanup_errors.push(format!("table catalog restore: {error}"));
            }
            if let Err(error) = self.reload_catalog_registries_after_rollback() {
                cleanup_errors.push(format!("registry restore: {error}"));
            }
        }
        if let Some(snapshot) = frame.session_savepoints.get(name).cloned() {
            self.restore_session_state(&snapshot);
        }
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

    fn snapshot_session_state(&self) -> SessionStateSnapshot {
        SessionStateSnapshot {
            search_path: self.search_path.read().clone(),
            session_vars: self.session_vars.read().clone(),
            random_state: *self.random_state.lock(),
            sequence_currvals: self.sequence_currvals.read().clone(),
            prepared: self.prepared.read().clone(),
            sql_statement_cache: self.sql_statement_cache.read().clone(),
        }
    }

    fn restore_session_state(&self, snapshot: &SessionStateSnapshot) {
        self.search_path.write().clone_from(&snapshot.search_path);
        *self.session_vars.write() = snapshot.session_vars.clone();
        *self.random_state.lock() = snapshot.random_state;
        *self.sequence_currvals.write() = snapshot.sequence_currvals.clone();
        *self.prepared.write() = snapshot.prepared.clone();
        *self.sql_statement_cache.write() = snapshot.sql_statement_cache.clone();
    }

    fn snapshot_transaction_data(&self) -> Result<Option<EngineDataSnapshot>, SQLError> {
        if self.backend.is_some() {
            return Ok(None);
        }
        let mut tables = BTreeMap::new();
        for (name, table) in self.tables.read().iter() {
            // Capture an already-writable deep copy once. Calling `snapshot`
            // and then probing `writable_snapshot` would allocate and discard
            // a second database-sized clone before the statement even starts.
            let document_store: std::sync::Arc<dyn uqa_storage::DocumentStore> =
                std::sync::Arc::from(table.document_store.read().writable_snapshot().map_err(
                    |err| Self::storage_tx_error("snapshot writable document store", &err),
                )?);
            let inverted_index: std::sync::Arc<dyn uqa_storage::InvertedIndex> =
                std::sync::Arc::from(table.inverted_index.read().writable_snapshot().map_err(
                    |err| Self::storage_tx_error("snapshot writable inverted index", &err),
                )?);
            let mut vector_indexes = BTreeMap::new();
            for (field, index) in table.vector_indexes.read().iter() {
                let snapshot: std::sync::Arc<dyn uqa_storage::VectorIndex> =
                    std::sync::Arc::from(index.writable_snapshot().map_err(|err| {
                        Self::storage_tx_error("snapshot writable vector index", &err)
                    })?);
                vector_indexes.insert(field.clone(), snapshot);
            }
            tables.insert(
                name.clone(),
                TableDataSnapshot {
                    state: table.clone(),
                    document_store,
                    inverted_index,
                    vector_indexes,
                    fts_fields: table.fts_fields.read().clone(),
                    columns: table.columns.read().clone(),
                    next_id: *table.next_id.lock(),
                    analyzer: table.analyzer.read().clone(),
                    column_stats: table.column_stats.read().clone(),
                    column_stats_loaded: table
                        .column_stats_loaded
                        .load(std::sync::atomic::Ordering::Acquire),
                    column_stats_dirty: table
                        .column_stats_dirty
                        .load(std::sync::atomic::Ordering::Acquire),
                    table_checks: table.table_checks.read().clone(),
                    foreign_keys: table.foreign_keys.read().clone(),
                    key_constraints: table.key_constraints.read().clone(),
                    doc_count_cache: table
                        .doc_count_cache
                        .load(std::sync::atomic::Ordering::Acquire),
                    doc_count_dirty: table
                        .doc_count_dirty
                        .load(std::sync::atomic::Ordering::Acquire),
                },
            );
        }
        Ok(Some(EngineDataSnapshot {
            tables,
            graphs: self.graphs.read().clone(),
            models: self.models.read().clone(),
            scoring_params: self.scoring_params.read().clone(),
            views: self.views.read().clone(),
            sequences: self.sequences.read().clone(),
            catalog_indexes: self.catalog_indexes.read().clone(),
            schemas: self.schemas.read().clone(),
            path_indexes: self.path_indexes.read().clone(),
            named_analyzers: self.named_analyzers.read().clone(),
            table_field_analyzers: self.table_field_analyzers.read().clone(),
            foreign_servers: self.foreign_servers.read().clone(),
            foreign_tables: self.foreign_tables.read().clone(),
            foreign_memory_tables: self.foreign_memory_tables.read().clone(),
            sql_user_functions: self.sql_user_functions.read().clone(),
        }))
    }

    /// Memory-engine rollback path: snapshots only exist when no
    /// persistent backend is attached, so these store operations run
    /// against in-memory stores. They are still fallible by signature;
    /// propagating keeps a (logic-bug) failure loud instead of leaving
    /// a half-restored engine behind a successful-looking rollback.
    fn restore_transaction_data(&self, snapshot: &EngineDataSnapshot) -> Result<(), SQLError> {
        {
            let mut tables = self.tables.write();
            tables.retain(|name, _| snapshot.tables.contains_key(name));
            for (name, table_snapshot) in &snapshot.tables {
                tables
                    .entry(name.clone())
                    .or_insert_with(|| table_snapshot.state.clone());
            }
        }
        for table_snapshot in snapshot.tables.values() {
            let table = &table_snapshot.state;
            let document_store = table_snapshot
                .document_store
                .writable_snapshot()
                .map_err(|err| Self::storage_tx_error("ROLLBACK document restore", &err))?;
            let inverted_index = table_snapshot
                .inverted_index
                .writable_snapshot()
                .map_err(|err| Self::storage_tx_error("ROLLBACK FTS restore", &err))?;
            let mut vector_indexes = BTreeMap::new();
            for (field, index) in &table_snapshot.vector_indexes {
                vector_indexes.insert(
                    field.clone(),
                    index
                        .writable_snapshot()
                        .map_err(|err| Self::storage_tx_error("ROLLBACK vector restore", &err))?,
                );
            }
            *table.document_store.write() = document_store;
            *table.inverted_index.write() = inverted_index;
            *table.vector_indexes.write() = vector_indexes;
            table
                .fts_fields
                .write()
                .clone_from(&table_snapshot.fts_fields);
            table.columns.write().clone_from(&table_snapshot.columns);
            *table.next_id.lock() = table_snapshot.next_id;
            *table.analyzer.write() = table_snapshot.analyzer.clone();
            *table.column_stats.write() = table_snapshot.column_stats.clone();
            table.column_stats_loaded.store(
                table_snapshot.column_stats_loaded,
                std::sync::atomic::Ordering::Release,
            );
            table.column_stats_dirty.store(
                table_snapshot.column_stats_dirty,
                std::sync::atomic::Ordering::Release,
            );
            table
                .table_checks
                .write()
                .clone_from(&table_snapshot.table_checks);
            table
                .foreign_keys
                .write()
                .clone_from(&table_snapshot.foreign_keys);
            table
                .key_constraints
                .write()
                .clone_from(&table_snapshot.key_constraints);
            Self::value_indexes_clear(table);
            table.doc_count_cache.store(
                table_snapshot.doc_count_cache,
                std::sync::atomic::Ordering::Release,
            );
            table.doc_count_dirty.store(
                table_snapshot.doc_count_dirty,
                std::sync::atomic::Ordering::Release,
            );
        }
        *self.graphs.write() = snapshot.graphs.clone();
        *self.models.write() = snapshot.models.clone();
        *self.scoring_params.write() = snapshot.scoring_params.clone();
        *self.views.write() = snapshot.views.clone();
        *self.sequences.write() = snapshot.sequences.clone();
        *self.catalog_indexes.write() = snapshot.catalog_indexes.clone();
        self.schemas.write().clone_from(&snapshot.schemas);
        *self.path_indexes.write() = snapshot.path_indexes.clone();
        *self.named_analyzers.write() = snapshot.named_analyzers.clone();
        *self.table_field_analyzers.write() = snapshot.table_field_analyzers.clone();
        *self.foreign_servers.write() = snapshot.foreign_servers.clone();
        *self.foreign_tables.write() = snapshot.foreign_tables.clone();
        *self.foreign_memory_tables.write() = snapshot.foreign_memory_tables.clone();
        *self.sql_user_functions.write() = snapshot.sql_user_functions.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn end_backend_transaction_early(engine: &Engine) {
        engine
            .backend
            .as_ref()
            .expect("persistent test engine")
            .rollback_transaction()
            .expect("end backend transaction early");
    }

    fn assert_combined_panic_and_rollback_error(error: &str) {
        assert!(error.contains("rollback"), "{error}");
        assert!(error.contains("original panic: callback panic"), "{error}");
    }

    #[test]
    fn implicit_read_transaction_rolls_back_an_unclassified_storage_write() {
        let directory = tempfile::tempdir().unwrap();
        let engine = Engine::open(&directory.path().join("read-only-guard.db")).unwrap();

        engine.begin_implicit_statement_transaction(true).unwrap();
        engine
            .sqlite_session
            .as_ref()
            .unwrap()
            .with(|connection| {
                connection.execute(
                    "INSERT OR REPLACE INTO _metadata (key, value) VALUES ('hidden-write', 'x')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let error = engine.commit().unwrap_err().to_string();
        assert!(error.contains("read-only SQL execution"), "{error}");
        assert_eq!(engine.transaction_depth(), 0);
        assert_eq!(
            engine
                .catalog
                .as_ref()
                .unwrap()
                .get_metadata("hidden-write")
                .unwrap(),
            None
        );
    }

    #[test]
    fn rollback_failure_after_callback_panic_is_returned_instead_of_panicking_again() {
        let directory = tempfile::tempdir().unwrap();

        let transaction_engine = Engine::open(&directory.path().join("transaction.db")).unwrap();
        let transaction_result: Result<(), SQLError> = transaction_engine.transaction(|engine| {
            end_backend_transaction_early(engine);
            panic!("callback panic");
        });
        let transaction_error = transaction_result.unwrap_err();
        assert_combined_panic_and_rollback_error(&transaction_error.to_string());
        assert_eq!(transaction_engine.transaction_depth(), 0);

        let storage_engine = Engine::open(&directory.path().join("storage.db")).unwrap();
        let storage_result: StorageBackendResult<()> = storage_engine
            .with_implicit_storage_transaction(|engine| {
                end_backend_transaction_early(engine);
                panic!("callback panic");
            });
        let storage_error = storage_result.unwrap_err();
        assert_combined_panic_and_rollback_error(&storage_error.to_string());
        assert_eq!(storage_engine.transaction_depth(), 0);

        let string_engine = Engine::open(&directory.path().join("string.db")).unwrap();
        let string_result: Result<(), String> =
            string_engine.with_implicit_string_transaction(|engine| {
                end_backend_transaction_early(engine);
                panic!("callback panic");
            });
        let string_error = string_result.unwrap_err();
        assert_combined_panic_and_rollback_error(&string_error);
        assert_eq!(string_engine.transaction_depth(), 0);
    }

    #[test]
    fn waiting_writer_refreshes_when_sqlite_commit_precedes_epoch_publication() {
        let directory = tempfile::tempdir().unwrap();
        let root = Engine::open(&directory.path().join("catalog-race.db")).unwrap();
        let writer = root.new_session().unwrap();
        let waiter = root.new_session().unwrap();

        writer.begin().unwrap();
        assert!(!waiter.has_schema("fresh").unwrap());
        writer.sql("CREATE SCHEMA fresh", &[]).unwrap();

        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let waiting_thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = waiter.sql("CREATE TABLE fresh.items (id INTEGER)", &[]);
            done_tx.send(result).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());

        // End the physical transaction without publishing the shared epoch.
        // This deterministically models the interval after SQLite COMMIT has
        // released its writer lock but before Engine::commit publishes it.
        writer
            .backend
            .as_ref()
            .unwrap()
            .commit_transaction()
            .unwrap();

        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        waiting_thread.join().unwrap();
        writer.tx_stack.lock().clear();
        assert!(root
            .new_session()
            .unwrap()
            .has_table("fresh.items")
            .unwrap());
    }

    #[test]
    fn unchanged_persistent_statements_keep_their_loaded_catalog_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let engine = Engine::open(&directory.path().join("stable-snapshot.db")).unwrap();
        engine
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .unwrap();

        // Consume the catalog/data generations published by CREATE TABLE.
        engine.sql("SELECT id FROM items", &[]).unwrap();
        let before = engine.require_table("items").unwrap();

        engine.sql("SELECT id FROM items", &[]).unwrap();
        let after = engine.require_table("items").unwrap();

        assert!(
            std::sync::Arc::ptr_eq(&before, &after),
            "an unchanged statement rebuilt the complete persistent catalog"
        );
    }

    #[test]
    fn pinned_reader_defers_sibling_catalog_epochs_until_transaction_end() {
        let directory = tempfile::tempdir().unwrap();
        let root = Engine::open(&directory.path().join("pinned-reader.db")).unwrap();
        let reader = root.new_session().unwrap();
        let writer = root.new_session().unwrap();

        {
            let mut stack = reader.tx_stack.lock();
            reader.begin_transaction_frame(&mut stack, true).unwrap();
        }
        assert!(!reader.has_schema("later").unwrap());
        writer.sql("CREATE SCHEMA later", &[]).unwrap();
        writer.create_graph("later_graph").unwrap();

        assert!(!reader.has_schema("later").unwrap());
        assert!(!reader.has_graph("later_graph").unwrap());
        reader.commit().unwrap();

        assert!(reader.has_schema("later").unwrap());
        assert!(reader.has_graph("later_graph").unwrap());
    }
}
