//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped transaction callbacks and implicit transaction lifecycle.

use super::{
    panic_description, BackendTransactionMode, Engine, SQLError, SQLParam, SQLResult,
    StorageBackendError, StorageBackendResult, TransactionScope,
};
use uqa_sql::SQLBatchError;

#[cfg(test)]
#[path = "batch_diagnostic_tests.rs"]
mod batch_diagnostic_tests;

#[derive(Clone, Copy)]
enum DefinitionAdmission {
    Direct,
    Statement,
}

impl Engine {
    /// Run `f` inside one engine transaction. An error or panic from `f` rolls back the transaction. A successful callback commits; an indeterminate commit retains its sealed attempt in the session for resolution through `commit` or `rollback`, without replaying `f`.
    pub fn transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        self.transaction_with_error(f, std::convert::identity)
    }

    pub(crate) fn transaction_with_error<R, E: std::fmt::Display>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, E>,
        map_transaction_error: impl Fn(SQLError) -> E,
    ) -> Result<R, E> {
        let _statement = self.lock_statement_gate();
        let scope = TransactionScope::begin(self).map_err(&map_transaction_error)?;
        self.run_transaction_scope(scope, f, map_transaction_error)
    }

    pub(super) fn run_transaction_scope<R, E: std::fmt::Display>(
        &self,
        mut scope: TransactionScope<'_>,
        f: impl FnOnce(&Self) -> Result<R, E>,
        map_transaction_error: impl Fn(SQLError) -> E,
    ) -> Result<R, E> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                scope.commit().map_err(&map_transaction_error)?;
                Ok(value)
            }
            Ok(Err(err)) => {
                if let Err(rollback_err) = scope.rollback() {
                    return Err(map_transaction_error(Self::rollback_cleanup_error(&rollback_err, format!(
                        "transaction rollback after error failed: {rollback_err}; original error: {err}"
                    ))));
                }
                Err(err)
            }
            Err(payload) => match scope.rollback() {
                Ok(()) => std::panic::resume_unwind(payload),
                Err(rollback_err) => Err(map_transaction_error(Self::rollback_cleanup_error(
                    &rollback_err,
                    format!(
                    "transaction rollback after panic failed: {rollback_err}; original panic: {}",
                    panic_description(payload.as_ref())
                ),
                ))),
            },
        }
    }

    /// Admit a public query through the active transaction or an owned implicit frame. Queries that can persist calibration or create a graph need a writable snapshot, including on memory engines. Attached physical readers retain their existing view and completion owner.
    pub(crate) fn with_query_transaction_snapshot<R, E: std::fmt::Display>(
        &self,
        select_snapshot: bool,
        read_only: bool,
        query: impl FnOnce(&Self) -> Result<R, E>,
        map_transaction_error: impl Fn(SQLError) -> E,
    ) -> Result<R, E> {
        if read_only && self.query_table_snapshots.is_some() {
            // Captured readers borrow the source session but already own their data and catalog view. Admitting another statement on that shared session would refresh its live transaction underneath the retained read.
            self.runtime
                .cancellation
                .check()
                .map_err(SQLError::from)
                .map_err(&map_transaction_error)?;
            return query(self);
        }
        if self.transaction_depth() != 0 {
            return self.run_existing_transaction_operation(
                || {
                    self.runtime
                        .cancellation
                        .check()
                        .map_err(SQLError::from)
                        .map_err(&map_transaction_error)?;
                    if select_snapshot {
                        self.prepare_explicit_statement_snapshot(true)
                            .map_err(&map_transaction_error)?;
                        self.mark_transaction_snapshot_set();
                    }
                    query(self)
                },
                &map_transaction_error,
            );
        }
        self.runtime
            .cancellation
            .check()
            .map_err(SQLError::from)
            .map_err(&map_transaction_error)?;
        if self
            .storage
            .backend
            .as_ref()
            .map_or(read_only, |backend| backend.in_transaction())
        {
            return query(self);
        }
        let scope = TransactionScope::begin_implicit_statement(self, read_only)
            .map_err(&map_transaction_error)?;
        self.run_transaction_scope(
            scope,
            |engine| {
                engine
                    .prepare_explicit_statement_snapshot(true)
                    .map_err(&map_transaction_error)?;
                engine.mark_transaction_snapshot_set();
                query(engine)
            },
            &map_transaction_error,
        )
    }

    /// Make a direct persistent-engine mutation atomic. A failed mutation in an existing transaction aborts its current frame or user savepoint through the same recovery boundary as SQL. Memory stores validate fallible vector input before their infallible writes; explicit memory transactions retain writable snapshots whose document and inverted-index state is copied on mutation. Avoiding a whole-engine snapshot for each direct memory insert keeps bulk ingestion linear.
    pub(crate) fn with_implicit_transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        self.with_implicit_mapped_transaction(f, std::convert::identity)
    }

    pub(super) fn with_implicit_transaction_mutation<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        let _statement = self.lock_statement_gate();
        if self.transaction_depth() != 0 {
            return self.run_existing_transaction_operation(|| f(self), std::convert::identity);
        }
        if self.storage.backend.is_none() {
            return f(self);
        }
        self.transaction(f)
    }

    /// Definition changes need transaction-owned relation locks even for direct memory APIs. Defer physical writer admission until execution has acquired and revalidated those locks.
    pub(crate) fn with_implicit_definition_transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        self.with_definition_transaction(DefinitionAdmission::Direct, f)
    }

    /// Run an admitted SQL definition's publication under the transaction owner. Direct definition APIs apply their own admission policy through the same boundary.
    pub(crate) fn with_definition_publication<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        self.with_definition_transaction(DefinitionAdmission::Statement, f)
    }

    fn with_definition_transaction<R>(
        &self,
        admission: DefinitionAdmission,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        let _statement = self.lock_statement_gate();
        let operation = |engine: &Self| {
            if matches!(admission, DefinitionAdmission::Direct)
                && engine.current_transaction_is_read_only()
            {
                return Err(SQLError::Routine {
                    sqlstate: "25006".into(),
                    message: "cannot execute direct mutation in a read-only transaction".into(),
                });
            }
            engine.prepare_serializable_transaction_snapshot()?;
            let value = f(engine)?;
            engine.publish_catalog_constraint_tuples()?;
            Ok(value)
        };
        if self.transaction_depth() != 0 {
            self.run_existing_transaction_operation(|| operation(self), std::convert::identity)
        } else {
            self.transaction(operation)
        }
    }

    /// Error-type-preserving counterpart for direct APIs whose public error
    /// type is not [`SQLError`]. `map_transaction_error` is used only for
    /// begin/commit/rollback infrastructure failures; an error returned by
    /// `f` is passed through unchanged when rollback succeeds.
    pub(crate) fn with_implicit_mapped_transaction<R, E>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, E>,
        map_transaction_error: impl Fn(SQLError) -> E,
    ) -> Result<R, E>
    where
        E: std::fmt::Display,
    {
        let _statement = self.lock_statement_gate();
        let existing_transaction = self.transaction_depth() != 0;
        let prepare_writer = existing_transaction || self.storage.backend.is_some();
        let operation = |engine: &Self| {
            if engine.current_transaction_is_read_only() {
                return Err(map_transaction_error(SQLError::Routine {
                    sqlstate: "25006".into(),
                    message: "cannot execute direct mutation in a read-only transaction".into(),
                }));
            }
            if prepare_writer {
                engine
                    .prepare_explicit_transaction_writer()
                    .map_err(&map_transaction_error)?;
            }
            f(engine)
        };
        if existing_transaction {
            return self
                .run_existing_transaction_operation(|| operation(self), &map_transaction_error);
        }
        if self.storage.backend.is_none() {
            return operation(self);
        }
        let scope = TransactionScope::begin(self).map_err(&map_transaction_error)?;
        self.run_transaction_scope(scope, operation, &map_transaction_error)
    }

    /// Storage-facing counterpart of [`Engine::with_implicit_transaction`].
    /// The storage error is retained verbatim when rollback succeeds so API
    /// callers can still classify the original backend failure.
    pub(crate) fn with_implicit_storage_transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        self.with_implicit_storage_transaction_inner(false, true, f)
    }

    /// Complete direct schema publication within the same storage transaction and preserve its public error type.
    pub(crate) fn with_implicit_storage_definition_transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        self.with_implicit_storage_transaction(|engine| {
            let result = f(engine)?;
            engine
                .publish_catalog_constraint_tuples()
                .map_err(|error| StorageBackendError::backend("constraint publication", error))?;
            Ok(result)
        })
    }

    /// Run storage maintenance that `PostgreSQL` permits in a read-only transaction. The transaction remains logically read-only, while its physical backend is allowed to persist maintenance metadata such as ANALYZE statistics.
    pub(crate) fn with_read_only_compatible_storage_transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        self.with_storage_maintenance_scope(|engine| {
            engine.prepare_storage_maintenance_writer()?;
            f(engine)
        })
    }

    /// Establish the maintenance transaction before execution acquires logical locks or samples rows. Physical write admission is deferred until the caller has finished every required logical wait.
    pub(crate) fn with_storage_maintenance_scope<R>(
        &self,
        f: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        self.with_implicit_storage_transaction_inner(true, false, f)
    }

    pub(crate) fn prepare_storage_maintenance_writer(&self) -> StorageBackendResult<()> {
        self.prepare_explicit_transaction_writer()
            .map_err(|error| StorageBackendError::backend("maintenance transaction", error))?;
        Ok(())
    }

    /// Automatic maintenance must release its relation locks instead of waiting behind a serialized writer that may need to upgrade those same locks.
    pub(crate) fn try_prepare_storage_maintenance_writer(&self) -> StorageBackendResult<bool> {
        let mark = self
            .session
            .transactions
            .lock()
            .first()
            .filter(|frame| frame.backend_mode == BackendTransactionMode::Deferred)
            .map(|frame| frame.begin_lock_mark);
        if let Some(mark) = mark {
            let admitted = self
                .row_locks
                .try_acquire_relation(
                    self.session_id,
                    self.row_locks.backend_writer_key(),
                    crate::row_locks::RelationLockMode::AccessExclusive,
                    mark,
                    &self.runtime.cancellation,
                )
                .map_err(|error| {
                    StorageBackendError::backend("maintenance writer admission", error)
                })?;
            if !admitted {
                return Ok(false);
            }
        }
        self.prepare_storage_maintenance_writer()?;
        Ok(true)
    }

    fn with_implicit_storage_transaction_inner<R>(
        &self,
        maintenance: bool,
        promote_writer: bool,
        f: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        let _statement = self.lock_statement_gate();
        let operation = |engine: &Self| {
            if !maintenance && engine.current_transaction_is_read_only() {
                return Err(StorageBackendError::backend(
                    "storage mutation",
                    SQLError::Routine {
                        sqlstate: "25006".into(),
                        message: "cannot execute storage mutation in a read-only transaction"
                            .into(),
                    },
                ));
            }
            engine
                .prepare_serializable_transaction_snapshot()
                .map_err(|error| StorageBackendError::backend("maintenance snapshot", error))?;
            if promote_writer {
                engine
                    .prepare_explicit_transaction_writer()
                    .map_err(|error| {
                        StorageBackendError::backend("promote storage transaction", error)
                    })?;
            }
            f(engine)
        };
        if self.transaction_depth() != 0 {
            return self.run_existing_transaction_operation(
                || operation(self),
                |error| StorageBackendError::backend("transaction abort", error),
            );
        }
        let scope = TransactionScope::begin(self).map_err(|error| {
            StorageBackendError::backend("begin implicit storage transaction", error)
        })?;
        self.run_transaction_scope(scope, operation, |error| {
            StorageBackendError::backend("storage transaction", error)
        })
    }

    pub(crate) fn with_implicit_string_transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, String>,
    ) -> Result<R, String> {
        let _statement = self.lock_statement_gate();
        if self.transaction_depth() != 0 {
            return self.run_existing_transaction_operation(
                || {
                    self.prepare_explicit_transaction_writer()
                        .map_err(|error| {
                            format!("promote explicit engine transaction failed: {error}")
                        })?;
                    f(self)
                },
                |error| error.to_string(),
            );
        }
        let mut scope = TransactionScope::begin(self)
            .map_err(|error| format!("begin implicit engine transaction failed: {error}"))?;
        if let Err(error) = self.prepare_explicit_transaction_writer() {
            let error = format!("promote implicit engine transaction failed: {error}");
            return match scope.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "rollback implicit engine transaction after promotion failure failed: {rollback_error}; original error: {error}"
                )),
            };
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                scope
                    .commit()
                    .map_err(|error| format!("commit implicit engine transaction failed: {error}"))?;
                Ok(value)
            }
            Ok(Err(error)) => match scope.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "rollback implicit engine transaction failed: {rollback_error}; original error: {error}"
                )),
            },
            Err(payload) => match scope.rollback() {
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
        self.sql_batch_diagnosed(statements)
            .map_err(SQLBatchError::into_error)
    }

    /// Execute the same atomic batch while retaining the zero-based failing member.
    /// Transaction-boundary failures carry no member; no statement is retried.
    pub fn sql_batch_diagnosed(
        &self,
        statements: &[(&str, &[SQLParam])],
    ) -> Result<Vec<SQLResult>, SQLBatchError> {
        self.sql_batch_observed(statements, |_, _| Ok(()))
    }

    /// Execute multiple SQL statements inside one engine transaction like [`Engine::sql_batch`], replacing each result's enum values by their labels before the next statement runs, so a later statement cannot remove a type an earlier result still names. Host bindings return these results.
    pub fn sql_batch_with_labels(
        &self,
        statements: &[(&str, &[SQLParam])],
    ) -> Result<Vec<SQLResult>, SQLError> {
        self.sql_batch_observed(statements, Engine::render_enum_labels)
            .map_err(SQLBatchError::into_error)
    }

    fn sql_batch_observed(
        &self,
        statements: &[(&str, &[SQLParam])],
        observe: impl Fn(&Engine, &mut SQLResult) -> Result<(), SQLError>,
    ) -> Result<Vec<SQLResult>, SQLBatchError> {
        self.transaction_with_error(
            |engine| {
                uqa_execution::statement::notifications::admit_sql_batch_diagnosed(
                    engine.notification_subscriptions_required(),
                    engine.parser_settings(),
                    statements.iter().map(|(sql, _)| *sql),
                    &engine.runtime.cancellation,
                )?;
                let mut results = Vec::with_capacity(statements.len());
                for (index, (sql, params)) in statements.iter().enumerate() {
                    let at_statement = |error| SQLBatchError::statement(error, index);
                    let mut result = engine.sql(sql, params).map_err(at_statement)?;
                    observe(engine, &mut result).map_err(at_statement)?;
                    results.push(result);
                }
                Ok(results)
            },
            SQLBatchError::transaction,
        )
    }
}
