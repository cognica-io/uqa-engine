//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped transaction callbacks and implicit transaction lifecycle.

use super::{
    panic_description, Engine, SQLError, SQLParam, SQLResult, StorageBackendError,
    StorageBackendResult, TransactionIntent, TransactionScope,
};

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
        let _statement = self.runtime.statement_gate.lock();
        let scope = TransactionScope::begin(self).map_err(&map_transaction_error)?;
        self.run_transaction_scope(scope, f, map_transaction_error)
    }

    fn run_transaction_scope<R, E: std::fmt::Display>(
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
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()
                .map_err(&map_transaction_error)?;
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
        let _statement = self.runtime.statement_gate.lock();
        if self.current_transaction_is_read_only() {
            return Err(SQLError::Routine {
                sqlstate: "25006".into(),
                message: "cannot execute direct mutation in a read-only transaction".into(),
            });
        }
        self.with_implicit_transaction_mutation(f)
    }

    pub(super) fn with_implicit_transaction_mutation<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()?;
            return self.run_existing_transaction_operation(
                || {
                    self.prepare_explicit_transaction_writer()?;
                    f(self)
                },
                std::convert::identity,
            );
        }
        if self.storage.backend.is_none() {
            return f(self);
        }
        self.transaction(|engine| {
            engine.prepare_explicit_transaction_writer()?;
            f(engine)
        })
    }

    /// Definition changes need transaction-owned relation locks even for direct memory APIs. Defer physical writer admission until execution has acquired and revalidated those locks.
    pub(crate) fn with_implicit_definition_transaction<R>(
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
            self.run_existing_transaction_operation(
                || {
                    self.prepare_serializable_transaction_snapshot()?;
                    f(self)
                },
                std::convert::identity,
            )
        } else {
            self.transaction(|engine| {
                engine.prepare_serializable_transaction_snapshot()?;
                f(engine)
            })
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
        let _statement = self.runtime.statement_gate.lock();
        if self.current_transaction_is_read_only() {
            return Err(map_transaction_error(SQLError::Routine {
                sqlstate: "25006".into(),
                message: "cannot execute direct mutation in a read-only transaction".into(),
            }));
        }
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()
                .map_err(&map_transaction_error)?;
            return self.run_existing_transaction_operation(
                || {
                    self.prepare_explicit_transaction_writer()
                        .map_err(&map_transaction_error)?;
                    f(self)
                },
                &map_transaction_error,
            );
        }
        if self.storage.backend.is_none() {
            return f(self);
        }
        let mut scope = TransactionScope::begin(self).map_err(&map_transaction_error)?;
        if let Err(error) = self.prepare_explicit_transaction_writer() {
            let error = map_transaction_error(error);
            return match scope.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(map_transaction_error(Self::rollback_cleanup_error(&rollback_error, format!(
                    "rollback implicit engine transaction after promotion failure failed: {rollback_error}; original error: {error}"
                )))),
            };
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                scope.commit().map_err(&map_transaction_error)?;
                Ok(value)
            }
            Ok(Err(error)) => match scope.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(map_transaction_error(Self::rollback_cleanup_error(&rollback_error, format!(
                    "rollback implicit engine transaction failed: {rollback_error}; original error: {error}"
                )))),
            },
            Err(payload) => match scope.rollback() {
                Ok(()) => std::panic::resume_unwind(payload),
                Err(rollback_error) => Err(map_transaction_error(Self::rollback_cleanup_error(&rollback_error, format!(
                    "rollback implicit engine transaction after panic failed: {rollback_error}; original panic: {}",
                    panic_description(payload.as_ref())
                )))),
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
        self.with_implicit_storage_transaction_inner(false, true, f)
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
        if self.current_transaction_is_read_only() {
            // SQL access remains read-only. Physical maintenance admission is retained until the transaction ends, including after savepoint undo.
            for frame in self.session.transactions.lock().iter_mut() {
                frame.intent = TransactionIntent::ReadWrite;
            }
        }
        Ok(())
    }

    fn with_implicit_storage_transaction_inner<R>(
        &self,
        maintenance_can_override_default_read_only: bool,
        promote_writer: bool,
        f: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        let _statement = self.runtime.statement_gate.lock();
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()
                .map_err(|error| StorageBackendError::backend("storage transaction", error))?;
            return self.run_existing_transaction_operation(
                || {
                    self.prepare_serializable_transaction_snapshot()
                        .map_err(|error| {
                            StorageBackendError::backend("maintenance snapshot", error)
                        })?;
                    if promote_writer {
                        self.prepare_explicit_transaction_writer()
                            .map_err(|error| {
                                StorageBackendError::backend("promote storage transaction", error)
                            })?;
                    }
                    f(self)
                },
                |error| StorageBackendError::backend("transaction abort", error),
            );
        }
        let mut scope = TransactionScope::begin(self).map_err(|error| {
            StorageBackendError::backend("begin implicit storage transaction", error)
        })?;
        if maintenance_can_override_default_read_only {
            if let Some(frame) = self.session.transactions.lock().last_mut() {
                frame.intent = TransactionIntent::ReadWrite;
                frame.characteristics.read_only = false;
            }
        }
        let preparation = self
            .prepare_serializable_transaction_snapshot()
            .and_then(|()| {
                promote_writer
                    .then(|| self.prepare_explicit_transaction_writer())
                    .transpose()
                    .map(|_| ())
            });
        if let Err(error) = preparation {
            let error = StorageBackendError::backend("prepare implicit storage transaction", error);
            return match scope.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(StorageBackendError::backend("rollback implicit storage transaction", Self::rollback_cleanup_error(&rollback_error, format!(
                    "rollback implicit engine transaction after promotion failure failed: {rollback_error}; original error: {error}"
                )))),
            };
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                scope.commit().map_err(|error| {
                    StorageBackendError::backend("commit implicit storage transaction", error)
                })?;
                Ok(value)
            }
            Ok(Err(error)) => match scope.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(StorageBackendError::backend("rollback implicit storage transaction", Self::rollback_cleanup_error(&rollback_error, format!(
                    "rollback implicit engine transaction failed: {rollback_error}; original error: {error}"
                )))),
            },
            Err(payload) => match scope.rollback() {
                Ok(()) => std::panic::resume_unwind(payload),
                Err(rollback_error) => Err(StorageBackendError::backend("rollback implicit storage transaction", Self::rollback_cleanup_error(&rollback_error, format!(
                    "rollback implicit engine transaction after panic failed: {rollback_error}; original panic: {}",
                    panic_description(payload.as_ref())
                )))),
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
        self.transaction(|engine| {
            let mut results = Vec::with_capacity(statements.len());
            for (sql, params) in statements {
                results.push(engine.sql(sql, params)?);
            }
            Ok(results)
        })
    }
}
