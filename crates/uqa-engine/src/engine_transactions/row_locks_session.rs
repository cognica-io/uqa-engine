//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-level integration between the SQL transaction stack and the row-lock manager: typed-mutation locking, statement-scoped recheck contexts, and change publication for tuple-local rechecks.

use super::{panic_description, BackendTransactionMode, Engine, SQLError, TransactionIntent};

impl Engine {
    /// Run one typed row mutation with the same relation/tuple locking order as SQL DML: logical locks first, backend-writer promotion second. This prevents typed APIs from bypassing `SELECT ... FOR UPDATE` and avoids a writer/row-lock inversion while waiting for another transaction.
    pub(crate) fn with_implicit_row_write_transaction<R>(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
        strength: uqa_sql::ast::LockStrength,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        if self.storage.backend.is_none() && self.transaction_depth() == 0 {
            return f(self);
        }
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()?;
        }

        let started = self.transaction_depth() == 0;
        if started {
            self.begin_implicit_statement_transaction(false)?;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.lock_relation(table, crate::row_locks::RelationLockMode::RowExclusive)?;
            match self.lock_row(
                table,
                doc_id,
                strength,
                uqa_sql::ast::LockWait::Block,
                table,
            )? {
                crate::row_locks::LockAcquire::Granted { .. } => {}
                crate::row_locks::LockAcquire::Skipped => {
                    return Err(SQLError::Internal(
                        "blocking typed row mutation unexpectedly skipped a row".into(),
                    ));
                }
            }
            self.prepare_explicit_transaction_writer()?;
            f(self)
        }));

        if !started {
            return match result {
                Ok(result) => result,
                Err(payload) => std::panic::resume_unwind(payload),
            };
        }
        match result {
            Ok(Ok(value)) => {
                self.commit()?;
                Ok(value)
            }
            Ok(Err(error)) => match self.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(SQLError::Internal(format!(
                    "typed row mutation failed: {error}; rollback also failed: {rollback_error}"
                ))),
            },
            Err(payload) => match self.rollback() {
                Ok(()) => std::panic::resume_unwind(payload),
                Err(rollback_error) => Err(SQLError::Internal(format!(
                    "typed row mutation rollback after panic failed: {rollback_error}; original panic: {}",
                    panic_description(payload.as_ref())
                ))),
            },
        }
    }

    fn row_lock_table_name(&self, table: &str) -> Result<String, SQLError> {
        self.try_resolve_table_name(table)
            .map_err(|error| {
                SQLError::Internal(format!("resolve row-lock table `{table}`: {error}"))
            })
            .map(|resolved| resolved.unwrap_or_else(|| table.to_string()))
    }

    pub(crate) fn row_lock_change_requires_recheck(&self) -> Result<bool, SQLError> {
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(true);
        };
        if self.storage.provider.is_none() {
            return Ok(false);
        }
        let stack = self.session.transactions.lock();
        let deferred_reader = stack.first().is_some_and(|frame| {
            frame.intent == TransactionIntent::ReadOnly
                || frame.backend_mode == BackendTransactionMode::Deferred
        });
        drop(stack);
        if !deferred_reader {
            return Ok(false);
        }
        backend
            .transaction_has_written()
            .map(|written| !written)
            .map_err(|error| Self::storage_tx_error("inspect row-lock recheck snapshot", &error))
    }

    pub(crate) fn lock_row(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
        strength: uqa_sql::ast::LockStrength,
        wait: uqa_sql::ast::LockWait,
        display_name: &str,
    ) -> Result<crate::row_locks::LockAcquire, SQLError> {
        let canonical = self.row_lock_table_name(table)?;
        let key = crate::row_locks::RowLockKey {
            table: self.row_locks.table_key(&canonical),
            doc_id,
        };
        self.row_locks.acquire(&crate::row_locks::LockRequest {
            session_id: self.session_id,
            key,
            strength,
            mark: self.current_lock_mark(),
            wait,
            cancel: &self.runtime.cancellation,
            relation: display_name,
        })
    }

    pub(crate) fn lock_relation(
        &self,
        table: &str,
        mode: crate::row_locks::RelationLockMode,
    ) -> Result<(), SQLError> {
        let canonical = self.row_lock_table_name(table)?;
        self.row_locks.acquire_relation(
            self.session_id,
            self.row_locks.table_key(&canonical),
            mode,
            self.current_lock_mark(),
            &self.runtime.cancellation,
        )
    }

    /// Whether the open transaction of this session already changed or rewrote the row itself. Such a row's current image is authoritative for this session, so cross-process verification must not replace it with the older committed image.
    pub(crate) fn row_changed_in_open_transaction(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<bool, SQLError> {
        let canonical = self.row_lock_table_name(table)?;
        let key = crate::row_locks::RowLockKey {
            table: self.row_locks.table_key(&canonical),
            doc_id,
        };
        Ok(self.session.transactions.lock().iter().any(|frame| {
            frame.row_changes.iter().any(|change| {
                change.key == key
                    || matches!(
                        change.kind,
                        crate::row_locks::PendingRowChangeKind::Rewrite(successor)
                            if successor == key
                    )
            })
        }))
    }

    /// The row identity a committed primary-key rewrite moved `doc_id` to, following chained rewrites to the final identity. `None` when the row keeps its identity in the latest committed state visible to the lock manager. Rewrites are recorded only while a lock or observer keeps them alive, which covers every DML statement waiting on the row.
    pub(crate) fn committed_row_successor(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<crate::row_locks::RowChangeTarget, SQLError> {
        let canonical = self.row_lock_table_name(table)?;
        self.row_locks.row_successor_after(
            &canonical,
            doc_id,
            self.row_lock_snapshot_change_baseline(),
        )
    }

    pub(crate) fn committed_physical_row_successor(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<crate::row_locks::PhysicalRowChangeTarget, SQLError> {
        let canonical = self.row_lock_table_name(table)?;
        self.row_locks.physical_row_successor_after(
            &canonical,
            doc_id,
            self.row_lock_snapshot_change_baseline(),
        )
    }

    pub(crate) fn row_lock_table_for_hash(&self, table_hash: u64) -> Result<String, SQLError> {
        let tables = self.storage.tables.read();
        let mut matches = tables
            .keys()
            .map(uqa_storage::RelationIdentity::qualified_name)
            .filter(|table| {
                crate::row_locks::RowLockManager::stable_table_hash(table) == table_hash
            });
        let table = matches.next().ok_or_else(|| {
            SQLError::Internal(format!(
                "row-change successor refers to unknown relation hash {table_hash}"
            ))
        })?;
        if matches.next().is_some() {
            return Err(SQLError::Internal(format!(
                "row-change successor relation hash {table_hash} is ambiguous"
            )));
        }
        Ok(table)
    }

    fn table_has_initially_deferred_foreign_key(&self, table: &str) -> Result<bool, SQLError> {
        self.try_foreign_keys(table)
            .map(|foreign_keys| {
                foreign_keys.iter().any(|foreign_key| {
                    foreign_key.enforced && foreign_key.deferrable && foreign_key.initially_deferred
                })
            })
            .map_err(|error| {
                SQLError::Internal(format!(
                    "read deferred foreign keys for table `{table}`: {error}"
                ))
            })
    }

    fn note_row_change(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
        kind: crate::row_locks::PendingRowChangeKind,
    ) -> Result<(), SQLError> {
        let canonical = self.row_lock_table_name(table)?;
        let key = crate::row_locks::RowLockKey {
            table: self.row_locks.table_key(&canonical),
            doc_id,
        };
        let track_deferred_foreign_key = !self.session.transactions.lock().is_empty()
            && self.table_has_initially_deferred_foreign_key(&canonical)?;
        let mut stack = self.session.transactions.lock();
        let change = crate::row_locks::PendingRowChange { key, kind };
        if let Some(frame) = stack.last_mut() {
            frame.row_changes.push(change);
            if track_deferred_foreign_key {
                frame.deferred_foreign_key_rows.insert(key);
            }
            Ok(())
        } else {
            drop(stack);
            let publication = self
                .row_locks
                .begin_change_publication(&self.runtime.cancellation)?;
            let result = self
                .row_locks
                .publish_row_changes(self.session_id, [change]);
            drop(publication);
            result
        }
    }

    pub(crate) fn note_row_inserted(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<(), SQLError> {
        self.note_row_change(
            table,
            doc_id,
            crate::row_locks::PendingRowChangeKind::Insert,
        )
    }

    pub(crate) fn note_row_changed(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<(), SQLError> {
        self.note_row_change(
            table,
            doc_id,
            crate::row_locks::PendingRowChangeKind::Update,
        )
    }

    pub(crate) fn note_row_deleted(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<(), SQLError> {
        self.note_row_change(
            table,
            doc_id,
            crate::row_locks::PendingRowChangeKind::Delete,
        )
    }

    pub(crate) fn defer_foreign_key_row(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<(), SQLError> {
        let canonical = self.row_lock_table_name(table)?;
        let key = crate::row_locks::RowLockKey {
            table: self.row_locks.table_key(&canonical),
            doc_id,
        };
        let mut stack = self.session.transactions.lock();
        let frame = stack.last_mut().ok_or_else(|| {
            SQLError::Internal("deferred foreign-key row outside a transaction".into())
        })?;
        frame.deferred_foreign_key_rows.insert(key);
        Ok(())
    }

    pub(crate) fn note_row_rewritten(
        &self,
        table: &str,
        old_doc_id: uqa_core::DocId,
        new_doc_id: uqa_core::DocId,
    ) -> Result<(), SQLError> {
        self.note_row_rewritten_between_tables(table, old_doc_id, table, new_doc_id)
    }

    pub(crate) fn note_row_rewritten_between_tables(
        &self,
        old_table: &str,
        old_doc_id: uqa_core::DocId,
        new_table: &str,
        new_doc_id: uqa_core::DocId,
    ) -> Result<(), SQLError> {
        let old_table = self.row_lock_table_name(old_table)?;
        let new_table = self.row_lock_table_name(new_table)?;
        let old = crate::row_locks::RowLockKey {
            table: self.row_locks.table_key(&old_table),
            doc_id: old_doc_id,
        };
        let new = crate::row_locks::RowLockKey {
            table: self.row_locks.table_key(&new_table),
            doc_id: new_doc_id,
        };
        let track_deferred_foreign_key = !self.session.transactions.lock().is_empty()
            && (self.table_has_initially_deferred_foreign_key(&old_table)?
                || self.table_has_initially_deferred_foreign_key(&new_table)?);
        let mut stack = self.session.transactions.lock();
        let change = crate::row_locks::PendingRowChange {
            key: old,
            kind: crate::row_locks::PendingRowChangeKind::Rewrite(new),
        };
        if let Some(frame) = stack.last_mut() {
            frame.row_changes.push(change);
            if track_deferred_foreign_key {
                frame.deferred_foreign_key_rows.insert(old);
                frame.deferred_foreign_key_rows.insert(new);
            }
            Ok(())
        } else {
            drop(stack);
            let publication = self
                .row_locks
                .begin_change_publication(&self.runtime.cancellation)?;
            let result = self
                .row_locks
                .publish_row_changes(self.session_id, [change]);
            drop(publication);
            result
        }
    }

    pub(crate) fn row_lock_manager(&self) -> std::sync::Arc<crate::row_locks::RowLockManager> {
        std::sync::Arc::clone(&self.row_locks)
    }

    /// Mark one in-flight SQL statement so every locking scope it executes, including scopes inside query-bearing commands, prepared execution, `EXPLAIN ANALYZE`, and DML sources, shares one row-lock recheck context. Host-callback statements nested inside another statement push their own frame.
    pub(crate) fn begin_row_lock_statement(&self) -> RowLockStatementScope<'_> {
        self.session.row_lock_statements.lock().push(None);
        RowLockStatementScope { engine: self }
    }

    /// The active statement's shared row-lock recheck context, created on first use with the statement snapshot's change epoch as its recheck baseline. A direct engine call outside any SQL statement owns an ephemeral context with the same baseline semantics.
    pub(crate) fn statement_row_lock_cache(
        &self,
    ) -> Result<std::sync::Arc<crate::sql::RowLockRetryCache>, SQLError> {
        let budget_bytes = self.work_mem_bytes()?;
        let baseline = self.row_lock_snapshot_change_baseline();
        let mut statements = self.session.row_lock_statements.lock();
        let Some(slot) = statements.last_mut() else {
            drop(statements);
            return Ok(std::sync::Arc::new(crate::sql::RowLockRetryCache::new(
                budget_bytes,
                self.row_lock_manager(),
                baseline,
            )));
        };
        if let Some(cache) = slot.as_ref() {
            return Ok(std::sync::Arc::clone(cache));
        }
        let cache = std::sync::Arc::new(crate::sql::RowLockRetryCache::new(
            budget_bytes,
            self.row_lock_manager(),
            baseline,
        ));
        *slot = Some(std::sync::Arc::clone(&cache));
        Ok(cache)
    }

    pub(crate) fn row_lock_snapshot_change_baseline(&self) -> crate::row_locks::RowChangeBaseline {
        self.session.transactions.lock().first().map_or_else(
            || crate::row_locks::RowChangeBaseline {
                epoch: self.row_locks.current_change_epoch(),
                cross_sequence: 0,
            },
            |frame| frame.snapshot_change_baseline,
        )
    }

    pub(crate) fn update_statement_row_lock_baseline(
        &self,
        baseline: crate::row_locks::RowChangeBaseline,
    ) {
        if let Some(Some(cache)) = self.session.row_lock_statements.lock().last() {
            cache.set_snapshot_baseline(baseline);
        }
    }

    pub(crate) fn rollback_row_lock_acquisition(
        &self,
        acquisition: crate::row_locks::RowLockAcquisition,
    ) {
        self.row_locks.rollback_acquisition(acquisition);
    }
}

/// RAII frame for one in-flight SQL statement's shared row-lock recheck context. Dropping the frame releases the statement's change observation so the lock manager can prune retained row-change versions.
pub(crate) struct RowLockStatementScope<'engine> {
    engine: &'engine Engine,
}

impl Drop for RowLockStatementScope<'_> {
    fn drop(&mut self) {
        self.engine.session.row_lock_statements.lock().pop();
    }
}
