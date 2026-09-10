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
    /// Run `f` inside one engine transaction. On success the transaction is
    /// committed; on error or panic it is rolled back before the error/panic is
    /// returned to the caller.
    pub fn transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let mut scope = TransactionScope::begin(self)?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                scope.commit()?;
                Ok(value)
            }
            Ok(Err(err)) => {
                if let Err(rollback_err) = scope.rollback() {
                    return Err(SQLError::Internal(format!(
                        "transaction rollback after error failed: {rollback_err}; original error: {err}"
                    )));
                }
                Err(err)
            }
            Err(payload) => match scope.rollback() {
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
        let mut scope = TransactionScope::begin(self).map_err(|error| {
            map_transaction_error(format!("begin implicit engine transaction failed: {error}"))
        })?;
        if let Err(error) = self.prepare_explicit_transaction_writer() {
            let error = map_transaction_error(format!(
                "promote implicit engine transaction failed: {error}"
            ));
            return match scope.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(map_transaction_error(format!(
                    "rollback implicit engine transaction after promotion failure failed: {rollback_error}; original error: {error}"
                ))),
            };
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                scope.commit().map_err(|error| {
                    map_transaction_error(format!(
                        "commit implicit engine transaction failed: {error}"
                    ))
                })?;
                Ok(value)
            }
            Ok(Err(error)) => match scope.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(map_transaction_error(format!(
                    "rollback implicit engine transaction failed: {rollback_error}; original error: {error}"
                ))),
            },
            Err(payload) => match scope.rollback() {
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
        let mut scope = TransactionScope::begin(self).map_err(|error| {
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
            return match scope.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(StorageBackendError::Other(format!(
                    "rollback implicit engine transaction after promotion failure failed: {rollback_error}; original error: {error}"
                ))),
            };
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                scope.commit().map_err(|error| {
                    StorageBackendError::Other(format!(
                        "commit implicit engine transaction failed: {error}"
                    ))
                })?;
                Ok(value)
            }
            Ok(Err(error)) => match scope.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(StorageBackendError::Other(format!(
                    "rollback implicit engine transaction failed: {rollback_error}; original error: {error}"
                ))),
            },
            Err(payload) => match scope.rollback() {
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
