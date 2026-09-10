//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public COPY stream entry points and session synchronization.
use crate::Engine;
use std::io::{Read, Write};
use uqa_sql::SQLError;
impl Engine {
    /// Consume a `PostgreSQL` text or CSV `COPY relation FROM STDIN` stream.
    ///
    /// The complete input is decoded before the single underlying multi-row insert starts. Row routing, defaults, identity allocation, generated columns, checks, foreign keys, and statement rollback consequently use exactly the same implementation as `INSERT`.
    pub fn copy_from(&self, statement: &str, mut input: impl Read) -> Result<u64, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let result = self.synchronize_for_copy().and_then(|()| {
            uqa_execution::copy::copy_from(&self.copy_execution_context(), statement, &mut input)
        });
        result.map_err(|error| self.abort_sql_transaction_after_error(error))
    }

    /// Write a `PostgreSQL` text or CSV `COPY ... TO STDOUT` stream and return the number of emitted rows.
    pub fn copy_to(&self, statement: &str, mut output: impl Write) -> Result<u64, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let result = self.synchronize_for_copy().and_then(|()| {
            uqa_execution::copy::copy_to(&self.copy_execution_context(), statement, &mut output)
        });
        result.map_err(|error| self.abort_sql_transaction_after_error(error))
    }

    fn synchronize_for_copy(&self) -> Result<(), SQLError> {
        self.synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("refresh table catalog: {error}")))?;
        self.synchronize_table_data().map_err(|error| {
            SQLError::Internal(format!("refresh committed table data: {error}"))
        })?;
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("refresh durable catalog registries: {error}"))
        })
    }
}
