//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, Engine, EngineDataSnapshot, SQLError, SQLParam, SQLResult, StorageBackendError,
    TableDataSnapshot, TransactionFrame,
};

impl Engine {
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
            Err(payload) => {
                let _ = self.rollback();
                std::panic::resume_unwind(payload);
            }
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
    pub fn close(&self) {
        // Roll back every open transaction.
        let had_open_transaction = {
            let mut guard = self.tx_stack.lock();
            let had_open_transaction = !guard.is_empty();
            guard.clear();
            had_open_transaction
        };
        if had_open_transaction {
            if let Some(backend) = self.backend.as_ref() {
                let _ = backend.rollback_transaction();
            }
        }
        // Clear FDW registries - closing connections is up to the
        // handler, but dropping the catalog entries is enough to free
        // the registered handles.
        self.foreign_servers.write().clear();
        self.foreign_tables.write().clear();
        self.foreign_memory_tables.write().clear();
    }

    pub fn run_transaction_statement(
        &self,
        tx: uqa_sql::ast::TransactionStmt,
    ) -> Result<(), SQLError> {
        use uqa_sql::ast::TransactionStmt;
        let mut guard = self.tx_stack.lock();
        match tx {
            TransactionStmt::Begin => self.begin_transaction_frame(&mut guard)?,
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

    fn begin_transaction_frame(&self, stack: &mut Vec<TransactionFrame>) -> Result<(), SQLError> {
        let storage_savepoint = if stack.is_empty() {
            if let Some(backend) = self.backend.as_ref() {
                backend
                    .begin_transaction()
                    .map_err(|err| Self::storage_tx_error("BEGIN", &err))?;
            }
            None
        } else {
            let savepoint = format!("__uqa_nested_tx_{}", stack.len());
            if let Some(backend) = self.backend.as_ref() {
                backend
                    .savepoint(&savepoint)
                    .map_err(|err| Self::storage_tx_error("nested BEGIN savepoint", &err))?;
            }
            Some(savepoint)
        };
        stack.push(TransactionFrame {
            storage_savepoint,
            savepoints: std::collections::BTreeSet::new(),
            data_snapshot: self.snapshot_transaction_data(),
            data_savepoints: BTreeMap::new(),
        });
        Ok(())
    }

    fn commit_transaction_frame(&self, stack: &mut Vec<TransactionFrame>) -> Result<(), SQLError> {
        let storage_savepoint = stack
            .last()
            .ok_or_else(|| SQLError::Internal("COMMIT without an open transaction".into()))?
            .storage_savepoint
            .clone();
        if let Some(backend) = self.backend.as_ref() {
            if let Some(savepoint) = storage_savepoint.as_ref() {
                backend
                    .release_savepoint(savepoint)
                    .map_err(|err| Self::storage_tx_error("nested COMMIT savepoint", &err))?;
            } else {
                backend
                    .commit_transaction()
                    .map_err(|err| Self::storage_tx_error("COMMIT", &err))?;
            }
        }
        stack.pop();
        Ok(())
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
            if let Some(savepoint) = storage_savepoint.as_ref() {
                backend
                    .rollback_to_savepoint(savepoint)
                    .map_err(|err| Self::storage_tx_error("nested ROLLBACK savepoint", &err))?;
                backend
                    .release_savepoint(savepoint)
                    .map_err(|err| Self::storage_tx_error("nested ROLLBACK release", &err))?;
            } else {
                backend
                    .rollback_transaction()
                    .map_err(|err| Self::storage_tx_error("ROLLBACK", &err))?;
            }
        }
        if let Some(snapshot) = stack.last().and_then(|frame| frame.data_snapshot.clone()) {
            self.restore_transaction_data(&snapshot)?;
        }
        stack.pop();
        Ok(())
    }

    fn save_transaction_savepoint(
        &self,
        stack: &mut [TransactionFrame],
        name: String,
    ) -> Result<(), SQLError> {
        let frame = stack
            .last_mut()
            .ok_or_else(|| SQLError::Internal("SAVEPOINT outside a transaction".into()))?;
        if let Some(backend) = self.backend.as_ref() {
            backend
                .savepoint(&name)
                .map_err(|err| Self::storage_tx_error("SAVEPOINT", &err))?;
        }
        if let Some(snapshot) = self.snapshot_transaction_data() {
            frame.data_savepoints.insert(name.clone(), snapshot);
        }
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
        frame.data_savepoints.remove(name);
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
        if let Some(snapshot) = frame.data_savepoints.get(name).cloned() {
            self.restore_transaction_data(&snapshot)?;
        }
        Ok(())
    }

    fn storage_tx_error(action: &str, err: &StorageBackendError) -> SQLError {
        SQLError::Internal(format!("{action} failed in storage backend: {err}"))
    }

    fn snapshot_transaction_data(&self) -> Option<EngineDataSnapshot> {
        if self.backend.is_some() {
            return None;
        }
        let mut tables = BTreeMap::new();
        for (name, table) in self.tables.read().iter() {
            let documents = table.document_store.read().iter_all().collect();
            let next_id = *table.next_id.lock();
            tables.insert(
                name.clone(),
                TableDataSnapshot {
                    state: table.clone(),
                    documents,
                    next_id,
                },
            );
        }
        Some(EngineDataSnapshot {
            tables,
            sequences: self.sequences_snapshot(),
        })
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
        for (name, table_snapshot) in &snapshot.tables {
            let Some(table) = self.table(name) else {
                continue;
            };
            table
                .document_store
                .write()
                .clear()
                .map_err(|err| Self::storage_tx_error("ROLLBACK data restore", &err))?;
            table
                .doc_count_dirty
                .store(true, std::sync::atomic::Ordering::Release);
            Self::value_indexes_clear(&table);
            table.inverted_index.write().clear();
            for index in table.vector_indexes.write().values_mut() {
                index.clear();
            }
            for (doc_id, document) in &table_snapshot.documents {
                let vectors = Self::document_vector_values(&table, document);
                self.add_document_with_vector_values(name, *doc_id, document.clone(), vectors)?;
            }
            *table.next_id.lock() = table_snapshot.next_id;
        }
        *self.sequences.write() = snapshot.sequences.clone();
        Ok(())
    }
}
