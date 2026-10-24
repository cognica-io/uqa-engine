//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine query entry points, statement gates, execution clocks and the statement thread context.

use uqa_execution::ColumnarBatch;

use crate::{Engine, SQLCursor, SQLCursorSummary};
use uqa_sql::{SQLError, SQLParam, SQLResult};

struct SQLExecutionScope<'a> {
    engine: &'a Engine,
    depth: &'a std::sync::atomic::AtomicUsize,
    statement_clock: &'a std::sync::atomic::AtomicI64,
    previous_clock: Option<i64>,
    /// The statement thread context, whose transaction start the statement's input functions and `now()` read and whose cancellation its evaluation observes, entered for the outermost statement on the executing thread; a nested statement keeps its caller's.
    _statement_thread: Option<uqa_execution::statement::thread_scope::StatementThreadScope>,
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
        let statement_thread = (!nested).then(|| engine.statement_thread_context().enter());
        (
            Self {
                engine,
                depth,
                statement_clock,
                previous_clock,
                _statement_thread: statement_thread,
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
        // A statement outside a transaction block runs in a transaction of its own, so the LOCAL assignments it made (`set_config(..., true)`) end with it.
        if previous == 1 && self.engine.transaction_depth() == 0 {
            self.engine.restore_local_runtime_parameters();
        }
    }
}

impl Engine {
    /// Bind retained regex state after provider initialization, which can install the native session allowance during initial catalog restoration.
    pub(crate) fn session_regex_cache(&self) -> std::sync::Arc<uqa_sql::expr::RegexCache> {
        let budget = self
            .storage
            .backend
            .as_ref()
            .and_then(|backend| backend.retention_control())
            .map_or_else(
                || self.session.query_retention.clone(),
                |control| control.memory().clone(),
            );
        let mut retained = self.session.regex_cache.lock();
        if let Some(cache) = retained
            .as_ref()
            .filter(|cache| cache.shares_allowance(&budget))
        {
            return cache.clone();
        }
        let cache = std::sync::Arc::new(uqa_sql::expr::RegexCache::new(budget));
        *retained = Some(cache.clone());
        cache
    }

    /// The thread context this session's statements evaluate under: its transaction clock, live date order and interval style, cancellation token and retained regex cache. Direct APIs that evaluate SQL expressions outside a statement enter the same context.
    pub(crate) fn statement_thread_context(
        &self,
    ) -> uqa_execution::statement::thread_context::StatementThreadContext {
        uqa_execution::statement::thread_context::StatementThreadContext {
            retained_rows:
                uqa_execution::query::sources::retained_source::RetainedRowContext::current(),
            regex_cache: Some(self.session_regex_cache()),
            temporal_input: uqa_sql::expr::TemporalInputContext {
                transaction_clock_micros: self.session_transaction_clock_micros(),
                interval_style: self.session.interval_style(),
                date_order: uqa_sql::semantics::parameters::datestyle::date_order(
                    &self.session.setting("DateStyle"),
                ),
            },
            cancellation: Some(self.runtime.cancellation.clone()),
        }
    }

    /// Public typed reads own a statement boundary; internal execution adapters retain their caller's existing scope instead.
    pub(crate) fn with_direct_read_snapshot<R>(
        &self,
        read: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        self.with_direct_query_snapshot(true, read, std::convert::identity)
    }

    pub(crate) fn with_catalog_read_snapshot<R>(
        &self,
        read: impl FnOnce(&Self) -> uqa_storage::StorageBackendResult<R>,
    ) -> uqa_storage::StorageBackendResult<R> {
        self.with_direct_query_snapshot(true, read, |error| {
            uqa_storage::StorageBackendError::backend("catalog query", error)
        })
    }

    pub(crate) fn with_direct_query_snapshot<R, E: std::fmt::Display>(
        &self,
        read_only: bool,
        query: impl FnOnce(&Self) -> Result<R, E>,
        map_transaction_error: impl Fn(SQLError) -> E,
    ) -> Result<R, E> {
        let _statement = self.lock_statement_gate();
        let (_execution, nested) = SQLExecutionScope::enter(self);
        self.with_query_transaction_snapshot(!nested, read_only, query, map_transaction_error)
    }

    /// Execute a simple-query message, delivering each statement's result in order. The whole message is parsed before execution, and implicit transaction segments retain the same boundaries as [`Engine::sql`]. The final result is delivered only after its implicit transaction has committed. Earlier results may have been delivered when a later statement fails; such failure still rolls back its implicit segment. An empty message delivers one result with no command completion.
    pub fn sql_simple_query(
        &self,
        query: &str,
        params: &[SQLParam],
        mut consume: impl FnMut(&SQLResult) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let _statement = self.lock_statement_gate();
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

    /// Execute a complete SQL message and return its final result. Multiple statements retain their native implicit transaction segments and explicit transaction boundaries; an empty message returns an empty result without a command tag.
    pub fn sql(&self, query: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
        let _statement = self.lock_statement_gate();
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

    pub(crate) fn execute_compiled_prepared(
        &self,
        name: &str,
        statement: std::sync::Arc<uqa_sql::Statement>,
        sql: Option<&str>,
        params: &[SQLParam],
    ) -> Result<SQLResult, SQLError> {
        let _statement = self.lock_statement_gate();
        let (_execution, nested) = SQLExecutionScope::enter(self);
        self.synchronize_table_catalog()
            .map_err(|err| SQLError::Internal(format!("refresh table catalog: {err}")))?;
        self.synchronize_table_data()
            .map_err(|err| SQLError::Internal(format!("refresh committed table data: {err}")))?;
        self.synchronize_catalog_registries().map_err(|err| {
            SQLError::Internal(format!("refresh durable catalog registries: {err}"))
        })?;
        uqa_execution::statement::batch::execute_prepared_statement(
            &self.batch_execution_context(),
            statement,
            name,
            sql,
            params,
            nested,
        )
    }

    /// Execute one read query into a bounded spill and return a columnar batch
    /// cursor. Unlike [`Engine::sql`], this path never retains the complete
    /// result as `Vec<ResultRow>` in memory. The statement finishes and its
    /// snapshot is committed before the cursor is returned.
    pub fn sql_cursor(&self, query: &str, params: &[SQLParam]) -> Result<SQLCursor, SQLError> {
        let _statement = self.lock_statement_gate();
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
