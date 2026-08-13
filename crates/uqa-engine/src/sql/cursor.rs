//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded SQL result cursor and its single-query execution boundary.

use std::sync::Arc;

use uqa_execution::{ColumnarBatch, SharedSpill, SharedSpillReader};
use uqa_planner::UnifiedPlan;
use uqa_sql::{compile, SQLError, SQLParam};

use super::driver::{rollback_after_statement_error, rollback_implicit_statement};
use super::{
    lower_statement, optimize_engine_plan, query_may_mutate_engine, Engine, UnifiedPlanExecutor,
};

/// Metadata known before a cursor is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLCursorSummary {
    pub columns: Vec<String>,
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
    pub(super) fn from_spill(columns: Vec<String>, spill: SharedSpill) -> Result<Self, SQLError> {
        let summary = SQLCursorSummary {
            columns,
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
    engine.cancellation_token().check()?;
    let cached = engine.cached_sql_statement(sql);
    let (statement, initial_plan, cached_optimized) = if let Some(cached) = cached {
        (
            cached.statement.as_ref().clone(),
            cached.logical_plan,
            cached.optimized_plan,
        )
    } else {
        let mut statements = compile(sql)?;
        if statements.len() != 1 {
            return Err(single_query_error(statements.len()));
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
    let query = query_from_plan(initial_plan.as_ref())?;

    if engine.transaction_depth() != 0 {
        let optimized = optimize_engine_plan(engine, initial_plan.as_ref().clone())?;
        return execute_spilled(engine, params, &optimized);
    }

    let is_read_query = !query_may_mutate_engine(engine, query)?;
    let needs_transaction = engine.storage.backend.is_some() || !is_read_query;
    if !needs_transaction {
        let optimized = if let Some(plan) = cached_optimized {
            plan
        } else {
            let plan = Arc::new(optimize_engine_plan(engine, initial_plan.as_ref().clone())?);
            engine.cache_optimized_sql_plan(sql, Arc::clone(&plan));
            plan
        };
        return execute_spilled(engine, params, optimized.as_ref());
    }

    engine.begin_implicit_statement_transaction(is_read_query)?;
    let mut plan = lower_statement(engine, statement.clone());
    engine.cache_sql_statement(
        sql.to_string(),
        Arc::new(statement.clone()),
        Arc::new(plan.clone()),
    );
    let must_restart_as_writer = if is_read_query && engine.storage.backend.is_some() {
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
        engine.cache_sql_statement(sql.to_string(), Arc::new(statement), Arc::new(plan.clone()));
    }
    let optimized = match optimize_engine_plan(engine, plan) {
        Ok(plan) => plan,
        Err(error) => return rollback_after_statement_error(engine, error),
    };
    let cursor = match execute_spilled(engine, params, &optimized) {
        Ok(cursor) => cursor,
        Err(error) => return rollback_after_statement_error(engine, error),
    };
    engine.run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit)?;
    Ok(cursor)
}

fn execute_spilled(
    engine: &Engine,
    params: &[SQLParam],
    plan: &UnifiedPlan,
) -> Result<SQLCursor, SQLError> {
    UnifiedPlanExecutor::new(engine, params)
        .execute_query_to_spill(plan)?
        .into_cursor()
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
