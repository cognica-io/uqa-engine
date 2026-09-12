//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine query entry points, statement gates and execution clocks.

use uqa_execution::ColumnarBatch;

use crate::{Engine, SQLCursor, SQLCursorSummary};
use uqa_sql::{SQLError, SQLParam, SQLResult};

struct SQLExecutionScope<'a> {
    depth: &'a std::sync::atomic::AtomicUsize,
    statement_clock: &'a std::sync::atomic::AtomicI64,
    previous_clock: Option<i64>,
}

impl<'a> SQLExecutionScope<'a> {
    fn enter(engine: &'a Engine) -> (Self, bool) {
        let depth = &engine.runtime.sql_execution_depth;
        let statement_clock = &engine.session.statement_started_at_micros;
        let nested = depth.fetch_add(1, std::sync::atomic::Ordering::Relaxed) != 0;
        let previous_clock = (!nested).then(|| {
            statement_clock.swap(
                uqa_sql::expr::clock_timestamp_micros(),
                std::sync::atomic::Ordering::Relaxed,
            )
        });
        (
            Self {
                depth,
                statement_clock,
                previous_clock,
            },
            nested,
        )
    }
}

impl Drop for SQLExecutionScope<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous_clock {
            self.statement_clock
                .store(previous, std::sync::atomic::Ordering::Relaxed);
        }
        let previous = self
            .depth
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        debug_assert!(previous != 0, "SQL execution depth underflow");
    }
}

impl Engine {
    /// Execute a simple-query message, delivering each statement's result in order. The whole message is parsed before execution, and implicit transaction segments retain the same boundaries as [`Engine::sql`]. The final result is delivered only after its implicit transaction has committed. Earlier results may have been delivered when a later statement fails; such failure still rolls back its implicit segment. An empty message delivers one result with no command completion.
    pub fn sql_simple_query(
        &self,
        query: &str,
        params: &[SQLParam],
        mut consume: impl FnMut(&SQLResult) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let (_execution, nested) = SQLExecutionScope::enter(self);
        self.synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("refresh table catalog: {error}")))?;
        self.synchronize_table_data().map_err(|error| {
            SQLError::Internal(format!("refresh committed table data: {error}"))
        })?;
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("refresh durable catalog registries: {error}"))
        })?;
        uqa_execution::statement::batch::execute_simple_query(
            &self.batch_execution_context(),
            query,
            params,
            nested,
            &mut consume,
        )
    }

    /// Run a single SQL statement against the engine.
    pub fn sql(&self, query: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let (_execution, nested) = SQLExecutionScope::enter(self);
        self.synchronize_table_catalog()
            .map_err(|err| SQLError::Internal(format!("refresh table catalog: {err}")))?;
        self.synchronize_table_data()
            .map_err(|err| SQLError::Internal(format!("refresh committed table data: {err}")))?;
        self.synchronize_catalog_registries().map_err(|err| {
            SQLError::Internal(format!("refresh durable catalog registries: {err}"))
        })?;
        if nested {
            uqa_execution::statement::batch::execute_nested(
                &self.batch_execution_context(),
                query,
                params,
            )
        } else {
            uqa_execution::statement::batch::execute(&self.batch_execution_context(), query, params)
        }
    }

    /// Execute one read query into a bounded spill and return a columnar batch
    /// cursor. Unlike [`Engine::sql`], this path never retains the complete
    /// result as `Vec<ResultRow>` in memory. The statement finishes and its
    /// snapshot is committed before the cursor is returned.
    pub fn sql_cursor(&self, query: &str, params: &[SQLParam]) -> Result<SQLCursor, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let (_execution, _) = SQLExecutionScope::enter(self);
        self.synchronize_table_catalog()
            .map_err(|err| SQLError::Internal(format!("refresh table catalog: {err}")))?;
        self.synchronize_table_data()
            .map_err(|err| SQLError::Internal(format!("refresh committed table data: {err}")))?;
        self.synchronize_catalog_registries().map_err(|err| {
            SQLError::Internal(format!("refresh durable catalog registries: {err}"))
        })?;
        uqa_execution::statement::cursor::execute(&self.batch_execution_context(), query, params)
    }

    /// Consume the bounded cursor synchronously without retaining batches.
    pub fn sql_columnar(
        &self,
        query: &str,
        params: &[SQLParam],
        mut consume: impl FnMut(ColumnarBatch) -> Result<(), SQLError>,
    ) -> Result<SQLCursorSummary, SQLError> {
        let mut cursor = self.sql_cursor(query, params)?;
        let summary = cursor.summary();
        for batch in &mut cursor {
            consume(batch?)?;
        }
        Ok(summary)
    }
}
