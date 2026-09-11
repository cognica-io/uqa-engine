//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single-query scheduling that seals a bounded result before committing its snapshot.

use super::{
    batch::context::BatchExecutionContext,
    plan_executor::UnifiedPlanExecutor,
    transactions::{
        abort_explicit_statement_error, rollback_after_statement_error, rollback_implicit_statement,
    },
};
use crate::query::{cursor::SQLCursor, locking::query_has_row_locks};
use std::sync::Arc;
use uqa_sql::semantics::effects::{query_may_mutate_engine, query_requires_statement_transaction};
use uqa_sql::{compile, plan::UnifiedPlan, SQLError, SQLParam};

pub fn execute<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    params: &[SQLParam],
) -> Result<SQLCursor, SQLError> {
    if let Err(error) = context.runtime.cancellation.check() {
        return Err(abort_explicit_statement_error(
            context.transactions,
            error.into(),
        ));
    }
    if !context.persistent_backend && context.transactions.transaction_depth() == 0 {
        if let Some(plan) = context.cache.cached_optimized_sql_plan(sql) {
            let executor =
                UnifiedPlanExecutor::new(context.statements.statement_execution_context(), params);
            return execute_spilled(&executor, plan.as_ref());
        }
    }
    execute_uncached_or_snapshot_scoped(context, sql, params)
}

#[inline(never)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves cursor bounds and position"
)]
fn execute_uncached_or_snapshot_scoped<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    params: &[SQLParam],
) -> Result<SQLCursor, SQLError> {
    let cached = context.cache.cached_sql_statement(sql);
    let (statement, initial_plan, cached_optimized) = if let Some(cached) = cached {
        (
            cached.statement.as_ref().clone(),
            cached.logical_plan,
            cached.optimized_plan,
        )
    } else {
        let mut statements = compile(sql)
            .map_err(|error| abort_explicit_statement_error(context.transactions, error))?;
        if statements.len() != 1 {
            return Err(abort_explicit_statement_error(
                context.transactions,
                single_query_error(statements.len()),
            ));
        }
        let statement = statements.remove(0);
        let plan = Arc::new(UnifiedPlan::lower_with(
            statement.clone(),
            context.aggregates,
        ));
        context.cache.cache_sql_statement(
            sql.to_string(),
            Arc::new(statement.clone()),
            Arc::clone(&plan),
        );
        (statement, plan, None)
    };
    let query = query_from_plan(initial_plan.as_ref())
        .map_err(|error| abort_explicit_statement_error(context.transactions, error))?;
    let has_row_locks = query_has_row_locks(query);
    let _row_lock_statement = has_row_locks.then(|| context.row_locks.begin_row_lock_statement());

    if context.transactions.transaction_depth() != 0 {
        context.transactions.ensure_transaction_usable()?;
        if has_row_locks {
            context
                .row_locks
                .statement_row_lock_cache()
                .map_err(|error| context.transactions.abort_after_error(error))?;
        }
        context
            .transactions
            .prepare_explicit_statement_snapshot(true)
            .map_err(|error| context.transactions.abort_after_error(error))?;
        let mut plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
        let current_query = query_from_plan(&plan)
            .map_err(|error| context.transactions.abort_after_error(error))?;
        context.cache.cache_sql_statement(
            sql.to_string(),
            Arc::new(statement.clone()),
            Arc::new(plan.clone()),
        );
        if query_may_mutate_engine(&context.effects.query_effect_context(), current_query)
            .map_err(|error| context.transactions.abort_after_error(error))?
            && !has_row_locks
            && context
                .transactions
                .prepare_explicit_transaction_writer()
                .map_err(|error| context.transactions.abort_after_error(error))?
        {
            plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
            query_from_plan(&plan)
                .map_err(|error| context.transactions.abort_after_error(error))?;
            context.cache.cache_sql_statement(
                sql.to_string(),
                Arc::new(statement.clone()),
                Arc::new(plan.clone()),
            );
        }
        let optimized = context
            .planning
            .plan_for_execution(plan, params)
            .map_err(|error| context.transactions.abort_after_error(error))?;
        let executor =
            UnifiedPlanExecutor::new(context.statements.statement_execution_context(), params);
        return execute_spilled(&executor, &optimized)
            .map_err(|error| context.transactions.abort_after_error(error));
    }

    let is_read_query = !query_may_mutate_engine(&context.effects.query_effect_context(), query)?;
    let requires_statement_transaction = !is_read_query
        || query_requires_statement_transaction(&context.effects.query_effect_context(), query)?;
    let needs_transaction =
        context.persistent_backend || requires_statement_transaction || has_row_locks;
    if !needs_transaction {
        let optimized = if let Some(plan) = cached_optimized {
            plan
        } else {
            let plan = Arc::new(
                context
                    .planning
                    .plan_for_execution(initial_plan.as_ref().clone(), params)?,
            );
            context
                .cache
                .cache_optimized_sql_plan(sql, Arc::clone(&plan));
            plan
        };
        let executor =
            UnifiedPlanExecutor::new(context.statements.statement_execution_context(), params);
        return execute_spilled(&executor, optimized.as_ref());
    }

    if has_row_locks {
        context.row_locks.statement_row_lock_cache()?;
    }
    context
        .transactions
        .begin_implicit_statement_transaction(is_read_query)?;
    let mut plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
    context.cache.cache_sql_statement(
        sql.to_string(),
        Arc::new(statement.clone()),
        Arc::new(plan.clone()),
    );
    let must_restart_as_writer = if is_read_query && context.persistent_backend && !has_row_locks {
        match query_from_plan(&plan).and_then(|query| {
            query_may_mutate_engine(&context.effects.query_effect_context(), query)
        }) {
            Ok(mutates) => mutates,
            Err(error) => return rollback_after_statement_error(context.transactions, error),
        }
    } else {
        false
    };
    if must_restart_as_writer {
        rollback_implicit_statement(
            context.transactions,
            "restart cursor read transaction as writer",
        )?;
        context
            .transactions
            .begin_implicit_statement_transaction(false)?;
        plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
        if let Err(error) = query_from_plan(&plan) {
            return rollback_after_statement_error(context.transactions, error);
        }
        context.cache.cache_sql_statement(
            sql.to_string(),
            Arc::new(statement.clone()),
            Arc::new(plan.clone()),
        );
    }
    let mutating_query = match query_from_plan(&plan)
        .and_then(|query| query_may_mutate_engine(&context.effects.query_effect_context(), query))
    {
        Ok(mutates) => mutates,
        Err(error) => return rollback_after_statement_error(context.transactions, error),
    };
    if mutating_query && context.persistent_backend && !has_row_locks {
        match context.transactions.prepare_explicit_transaction_writer() {
            Ok(true) => {
                plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
                if let Err(error) = query_from_plan(&plan) {
                    return rollback_after_statement_error(context.transactions, error);
                }
                context.cache.cache_sql_statement(
                    sql.to_string(),
                    Arc::new(statement.clone()),
                    Arc::new(plan.clone()),
                );
            }
            Ok(false) => {}
            Err(error) => return rollback_after_statement_error(context.transactions, error),
        }
    }
    let optimized = match context.planning.plan_for_execution(plan, params) {
        Ok(plan) => plan,
        Err(error) => return rollback_after_statement_error(context.transactions, error),
    };
    let executor =
        UnifiedPlanExecutor::new(context.statements.statement_execution_context(), params);
    let cursor = match execute_spilled(&executor, &optimized) {
        Ok(cursor) => cursor,
        Err(error) => return rollback_after_statement_error(context.transactions, error),
    };
    context
        .transactions
        .run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit)?;
    Ok(cursor)
}

fn execute_spilled<S: Clone + Send + Sync + 'static>(
    executor: &UnifiedPlanExecutor<'_, '_, S>,
    plan: &UnifiedPlan,
) -> Result<SQLCursor, SQLError> {
    executor.execute_query_to_spill(plan)?.into_cursor()
}

fn query_from_plan(plan: &UnifiedPlan) -> Result<&uqa_sql::plan::QueryPlan, SQLError> {
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
