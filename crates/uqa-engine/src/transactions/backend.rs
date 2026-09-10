//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred backend transactions, statement snapshots, and writer promotion.

use std::collections::BTreeMap;

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
        self.release_backend_reader_before_lock_wait(&mut stack)?;
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
            let (snapshot, graph_snapshot) = if backend.supports_concurrent_pinned_read_and_write()
            {
                let snapshot: std::sync::Arc<Engine> =
                    self.open_independent_pinned_read_snapshot()?.into();
                let uqa_graph::GraphStoreHandle::Persistent(graph_store) = snapshot
                    .new_graph_store()
                    .map_err(|error| SQLError::Internal(error.to_string()))?
                else {
                    return Err(SQLError::Internal(
                        "persistent snapshot has no graph storage".into(),
                    ));
                };
                let graph_store = graph_store.retain_resource(snapshot.clone());
                (FixedTransactionSnapshot::Pinned(snapshot), graph_store)
            } else {
                let snapshot = self.capture_detached_fixed_transaction_snapshot()?;
                let graph_snapshot =
                    self.detach_graph_storage_snapshot(&self.visible_graph_handles())?;
                self.restart_unwritten_backend_reader(&mut stack)?;
                (FixedTransactionSnapshot::Detached(snapshot), graph_snapshot)
            };
            self.install_fixed_graph_snapshot(&graph_snapshot)?;
            // FirstSnapshotSet belongs to the outer transaction even when
            // the first read occurs inside an existing SQL savepoint. Those
            // savepoints must restore the fixed graph view, not a live view.
            let graph_overlay = self.session.state.read().graph_overlay.clone();
            for (index, frame) in stack.iter_mut().enumerate() {
                if index != 0 {
                    frame
                        .session_snapshot
                        .graph_overlay
                        .clone_from(&graph_overlay);
                }
                for savepoint in &mut frame.savepoints {
                    savepoint
                        .session_snapshot
                        .graph_overlay
                        .clone_from(&graph_overlay);
                }
            }
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

    /// Restart an unwritten backend transaction without pinning or reloading caches. The logical transaction and its detached snapshot stay open while rollback-journal storage releases its reader lock.
    fn restart_unwritten_backend_reader(
        &self,
        stack: &mut Vec<TransactionFrame>,
    ) -> Result<(), SQLError> {
        let backend = self.storage.backend.as_ref().ok_or_else(|| {
            SQLError::Internal("restarting a backend reader requires persistent storage".into())
        })?;
        if stack.is_empty()
            || backend
                .transaction_has_written()
                .map_err(|error| Self::storage_tx_error("inspect backend reader", &error))?
        {
            return Err(SQLError::Internal(
                "restarting a backend reader requires an open transaction without writes".into(),
            ));
        }
        if let Err(error) = backend.rollback_transaction() {
            let failure = Self::storage_tx_error("release backend reader", &error);
            return Err(self.abort_failed_backend_transaction_replacement(
                stack,
                backend.as_ref(),
                failure,
            ));
        }
        if let Err(error) = backend.begin_read_transaction() {
            let failure = Self::storage_tx_error("restart backend reader transaction", &error);
            return Err(self.abort_failed_backend_transaction_replacement(
                stack,
                backend.as_ref(),
                failure,
            ));
        }
        stack[0].backend_mode = BackendTransactionMode::Deferred;
        Ok(())
    }

    fn release_backend_reader_before_lock_wait(
        &self,
        stack: &mut Vec<TransactionFrame>,
    ) -> Result<(), SQLError> {
        if self
            .storage
            .backend
            .as_ref()
            .is_some_and(|backend| !backend.supports_concurrent_pinned_read_and_write())
        {
            // A committing writer can own the logical lock while waiting for
            // SQLite readers. Never retain our reader while waiting for it.
            self.restart_unwritten_backend_reader(stack)?;
        }
        Ok(())
    }

    /// Release an unwritten backend reader before an autonomous maintenance write. The replacement is a bare deferred transaction, so the SQL transaction remains open without retaining a rollback-journal read lock.
    pub(crate) fn release_backend_reader_for_independent_maintenance(
        &self,
    ) -> Result<(), SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let mut stack = self.session.transactions.lock();
        self.restart_unwritten_backend_reader(&mut stack)
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

    /// Before a deferred writer statement reports a catalog lookup miss, wait for any backend writer that may have committed without publishing its in-process epoch yet, then refresh the `READ COMMITTED` snapshot without retaining writer ownership. The transient lock mark preserves row-lock-before-writer ordering for query-bearing commands.
    pub(crate) fn fence_catalog_writer_and_refresh_snapshot(&self) -> Result<(), SQLError> {
        if !self.backend_transaction_is_deferred() {
            return Ok(());
        }
        let (keep_mark, fence_mark) = {
            let mut stack = self.session.transactions.lock();
            self.release_backend_reader_before_lock_wait(&mut stack)?;
            let frame = stack.last_mut().ok_or_else(|| {
                SQLError::Internal("catalog writer fence requires an open transaction".into())
            })?;
            let keep_mark = frame.lock_mark;
            let fence_mark = frame.next_lock_mark;
            frame.next_lock_mark = fence_mark
                .checked_add(1)
                .ok_or_else(|| SQLError::Internal("transaction lock mark exhausted".into()))?;
            if fence_mark <= keep_mark {
                return Err(SQLError::Internal(
                    "catalog writer fence did not allocate a newer lock mark".into(),
                ));
            }
            (keep_mark, fence_mark)
        };
        let fence = self.row_locks.acquire_relation(
            self.session_id,
            self.row_locks.backend_writer_key(),
            crate::row_locks::RelationLockMode::AccessExclusive,
            fence_mark,
            &self.runtime.cancellation,
        );
        self.row_locks
            .release_mark_above(self.session_id, keep_mark);
        fence?;
        self.refresh_explicit_statement_snapshot()
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
        self.release_backend_reader_before_lock_wait(stack)?;
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
        // INSERT/COPY reserve document IDs while staging rows, before taking
        // the backend writer. Both key-lock rechecks and writer promotion can
        // rebuild table handles after a concurrent commit, from physical rows
        // that do not yet include those reservations. Preserve them for the
        // same physical table lifetime, never across DROP/recreate. Actual
        // transaction/savepoint rollback uses its separate restoration path.
        let reservations = self
            .storage
            .tables
            .read()
            .values()
            .map(|table| (table.storage_generation(), *table.next_id.lock()))
            .collect::<BTreeMap<_, _>>();
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
        for table in self.storage.tables.read().values() {
            if let Some(reserved) = reservations.get(&table.storage_generation()) {
                let mut next = table.next_id.lock();
                *next = (*next).max(*reserved);
            }
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
