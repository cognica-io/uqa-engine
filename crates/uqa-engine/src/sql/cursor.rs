//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded SQL result cursor and its single-query execution boundary.

use std::sync::Arc;

use uqa_execution::{ColumnarBatch, SharedSpill, SharedSpillReader};
use uqa_planner::UnifiedPlan;
use uqa_sql::{compile, ColumnType, SQLError, SQLParam};

use super::driver::{
    abort_explicit_statement_error, rollback_after_statement_error, rollback_implicit_statement,
};
use super::{
    lower_statement, optimize_engine_plan, query_has_row_locks, query_may_mutate_engine,
    query_requires_statement_transaction, Engine, UnifiedPlanExecutor,
};

/// Metadata known before a cursor is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLCursorSummary {
    pub columns: Vec<String>,
    pub column_types: Vec<Option<ColumnType>>,
    pub row_count: usize,
    pub spilled_to_disk: bool,
}

/// Iterator over schema-ordered column batches backed by a work-mem-bounded
/// [`SharedSpill`]. Dropping the cursor releases its temporary file. Positional
/// conversion preserves separately-valued duplicate labels in [`ColumnarBatch`].
pub struct SQLCursor {
    summary: SQLCursorSummary,
    reader: SharedSpillReader,
}

impl SQLCursor {
    pub(super) fn from_spill(
        columns: Vec<String>,
        column_types: Vec<Option<ColumnType>>,
        spill: SharedSpill,
    ) -> Result<Self, SQLError> {
        debug_assert_eq!(columns.len(), column_types.len());
        let summary = SQLCursorSummary {
            columns,
            column_types,
            row_count: spill.rows(),
            spilled_to_disk: spill.has_spilled(),
        };
        let reader = spill
            .into_reader()
            .map_err(super::select::physical_exec_error)?;
        Ok(Self { summary, reader })
    }

    pub fn columns(&self) -> &[String] {
        &self.summary.columns
    }

    pub fn column_types(&self) -> &[Option<ColumnType>] {
        &self.summary.column_types
    }

    pub fn row_count(&self) -> usize {
        self.summary.row_count
    }

    pub fn spilled_to_disk(&self) -> bool {
        self.summary.spilled_to_disk
    }

    pub fn summary(&self) -> SQLCursorSummary {
        self.summary.clone()
    }
}

impl Iterator for SQLCursor {
    type Item = Result<ColumnarBatch, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let batch = match self.reader.next()? {
                Ok(batch) => batch,
                Err(error) => return Some(Err(super::select::physical_exec_error(error))),
            };
            if !batch.is_empty() {
                return Some(Ok(ColumnarBatch::from_batch(&self.summary.columns, batch)));
            }
        }
    }
}

pub(super) fn execute(
    engine: &Engine,
    sql: &str,
    params: &[SQLParam],
) -> Result<SQLCursor, SQLError> {
    if let Err(error) = engine.cancellation_token().check() {
        return Err(abort_explicit_statement_error(engine, error.into()));
    }
    if engine.storage.backend.is_none() && engine.transaction_depth() == 0 {
        if let Some(plan) = engine.cached_optimized_sql_plan(sql) {
            let executor = UnifiedPlanExecutor::new(engine, params);
            return execute_spilled(&executor, plan.as_ref());
        }
    }
    execute_uncached_or_snapshot_scoped(engine, sql, params)
}

#[inline(never)]
fn execute_uncached_or_snapshot_scoped(
    engine: &Engine,
    sql: &str,
    params: &[SQLParam],
) -> Result<SQLCursor, SQLError> {
    let cached = engine.cached_sql_statement(sql);
    let (statement, initial_plan, cached_optimized) = if let Some(cached) = cached {
        (
            cached.statement.as_ref().clone(),
            cached.logical_plan,
            cached.optimized_plan,
        )
    } else {
        let mut statements =
            compile(sql).map_err(|error| abort_explicit_statement_error(engine, error))?;
        if statements.len() != 1 {
            return Err(abort_explicit_statement_error(
                engine,
                single_query_error(statements.len()),
            ));
        }
        let statement = statements.remove(0);
        let plan = Arc::new(lower_statement(engine, statement.clone()));
        engine.cache_sql_statement(
            sql.to_string(),
            Arc::new(statement.clone()),
            Arc::clone(&plan),
        );
        (statement, plan, None)
    };
    let query = query_from_plan(initial_plan.as_ref())
        .map_err(|error| abort_explicit_statement_error(engine, error))?;
    let has_row_locks = query_has_row_locks(query);
    let _row_lock_statement = has_row_locks.then(|| engine.begin_row_lock_statement());
    let executor = UnifiedPlanExecutor::new(engine, params);

    if engine.transaction_depth() != 0 {
        engine.ensure_transaction_usable()?;
        if has_row_locks {
            engine
                .statement_row_lock_cache()
                .map_err(|error| engine.abort_sql_transaction_after_error(error))?;
        }
        engine
            .prepare_explicit_statement_snapshot(true)
            .map_err(|error| engine.abort_sql_transaction_after_error(error))?;
        let mut plan = lower_statement(engine, statement.clone());
        let current_query = query_from_plan(&plan)
            .map_err(|error| engine.abort_sql_transaction_after_error(error))?;
        engine.cache_sql_statement(
            sql.to_string(),
            Arc::new(statement.clone()),
            Arc::new(plan.clone()),
        );
        if query_may_mutate_engine(engine, current_query)
            .map_err(|error| engine.abort_sql_transaction_after_error(error))?
            && !has_row_locks
            && engine
                .prepare_explicit_transaction_writer()
                .map_err(|error| engine.abort_sql_transaction_after_error(error))?
        {
            plan = lower_statement(engine, statement.clone());
            query_from_plan(&plan)
                .map_err(|error| engine.abort_sql_transaction_after_error(error))?;
            engine.cache_sql_statement(
                sql.to_string(),
                Arc::new(statement.clone()),
                Arc::new(plan.clone()),
            );
        }
        let optimized = optimize_engine_plan(engine, plan)
            .map_err(|error| engine.abort_sql_transaction_after_error(error))?;
        return execute_spilled(&executor, &optimized)
            .map_err(|error| engine.abort_sql_transaction_after_error(error));
    }

    let is_read_query = !query_may_mutate_engine(engine, query)?;
    let requires_statement_transaction =
        !is_read_query || query_requires_statement_transaction(engine, query)?;
    let needs_transaction =
        engine.storage.backend.is_some() || requires_statement_transaction || has_row_locks;
    if !needs_transaction {
        let optimized = if let Some(plan) = cached_optimized {
            plan
        } else {
            let plan = Arc::new(optimize_engine_plan(engine, initial_plan.as_ref().clone())?);
            engine.cache_optimized_sql_plan(sql, Arc::clone(&plan));
            plan
        };
        return execute_spilled(&executor, optimized.as_ref());
    }

    if has_row_locks {
        engine.statement_row_lock_cache()?;
    }
    engine.begin_implicit_statement_transaction(is_read_query)?;
    let mut plan = lower_statement(engine, statement.clone());
    engine.cache_sql_statement(
        sql.to_string(),
        Arc::new(statement.clone()),
        Arc::new(plan.clone()),
    );
    let must_restart_as_writer =
        if is_read_query && engine.storage.backend.is_some() && !has_row_locks {
            match query_from_plan(&plan).and_then(|query| query_may_mutate_engine(engine, query)) {
                Ok(mutates) => mutates,
                Err(error) => return rollback_after_statement_error(engine, error),
            }
        } else {
            false
        };
    if must_restart_as_writer {
        rollback_implicit_statement(engine, "restart cursor read transaction as writer")?;
        engine.begin_implicit_statement_transaction(false)?;
        plan = lower_statement(engine, statement.clone());
        if let Err(error) = query_from_plan(&plan) {
            return rollback_after_statement_error(engine, error);
        }
        engine.cache_sql_statement(
            sql.to_string(),
            Arc::new(statement.clone()),
            Arc::new(plan.clone()),
        );
    }
    let mutating_query =
        match query_from_plan(&plan).and_then(|query| query_may_mutate_engine(engine, query)) {
            Ok(mutates) => mutates,
            Err(error) => return rollback_after_statement_error(engine, error),
        };
    if mutating_query && engine.storage.backend.is_some() && !has_row_locks {
        match engine.prepare_explicit_transaction_writer() {
            Ok(true) => {
                plan = lower_statement(engine, statement.clone());
                if let Err(error) = query_from_plan(&plan) {
                    return rollback_after_statement_error(engine, error);
                }
                engine.cache_sql_statement(
                    sql.to_string(),
                    Arc::new(statement.clone()),
                    Arc::new(plan.clone()),
                );
            }
            Ok(false) => {}
            Err(error) => return rollback_after_statement_error(engine, error),
        }
    }
    let optimized = match optimize_engine_plan(engine, plan) {
        Ok(plan) => plan,
        Err(error) => return rollback_after_statement_error(engine, error),
    };
    let cursor = match execute_spilled(&executor, &optimized) {
        Ok(cursor) => cursor,
        Err(error) => return rollback_after_statement_error(engine, error),
    };
    engine.run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit)?;
    Ok(cursor)
}

fn execute_spilled(
    executor: &UnifiedPlanExecutor<'_, '_>,
    plan: &UnifiedPlan,
) -> Result<SQLCursor, SQLError> {
    executor.execute_query_to_spill(plan)?.into_cursor()
}

fn query_from_plan(plan: &UnifiedPlan) -> Result<&uqa_planner::QueryPlan, SQLError> {
    let UnifiedPlan::Query(query) = plan else {
        return Err(single_query_error(1));
    };
    Ok(query)
}

fn single_query_error(statement_count: usize) -> SQLError {
    SQLError::Unsupported(format!(
        "SQL cursor accepts exactly one query statement, received {statement_count}"
    ))
}
