//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred backend transactions, statement snapshots, and writer promotion.

use super::{BackendTransactionMode, Engine, FixedTransactionSnapshot, SQLError, TransactionFrame};

impl Engine {
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
}
