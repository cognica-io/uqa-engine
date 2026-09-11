//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL batch scheduling over live cache, transaction and statement inputs.

use super::{
    plan_executor::UnifiedPlanExecutor,
    transactions::{
        abort_explicit_statement_error, rollback_after_statement_error, rollback_implicit_statement,
    },
};
use crate::query::locking::query_has_row_locks;
use context::BatchExecutionContext;
use std::sync::Arc;
use uqa_sql::{
    plan::UnifiedPlan,
    semantics::effects::{
        is_transaction_control, query_may_mutate_engine, query_requires_statement_transaction,
        transaction_blocks::{no_active_transaction_error, transaction_requires_explicit_block},
    },
    SQLError, SQLParam, SQLResult,
};

pub mod context;

#[cfg(test)]
mod tests;

pub fn execute<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    execute_with_context(context, sql, params, false, &mut None)
}

pub fn execute_nested<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    execute_with_context(context, sql, params, true, &mut None)
}

type ResultConsumer<'a> = Option<&'a mut dyn FnMut(&SQLResult) -> Result<(), SQLError>>;

enum StatementInput<'sql> {
    Cached(Arc<uqa_sql::Statement>),
    Parsed(uqa_sql::ParsedStatement<'sql>),
}

impl StatementInput<'_> {
    fn compile(self) -> Result<uqa_sql::Statement, SQLError> {
        match self {
            Self::Cached(statement) => Ok(statement.as_ref().clone()),
            Self::Parsed(statement) => statement.compile(),
        }
    }
}

pub fn execute_simple_query<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    params: &[SQLParam],
    nested_statement: bool,
    consume: &mut dyn FnMut(&SQLResult) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let mut consumer: ResultConsumer<'_> = Some(consume);
    let result = execute_with_context(context, sql, params, nested_statement, &mut consumer)?;
    consume_result(context, &result, &mut consumer)
}

fn consume_result<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    result: &SQLResult,
    consumer: &mut ResultConsumer<'_>,
) -> Result<(), SQLError> {
    if let Some(consume) = consumer {
        consume(result)
            .map_err(|error| abort_explicit_statement_error(context.transactions, error))?;
    }
    Ok(())
}

fn execute_with_context<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    params: &[SQLParam],
    nested_statement: bool,
    consumer: &mut ResultConsumer<'_>,
) -> Result<SQLResult, SQLError> {
    // Reject cancelled tokens up-front so a stale cancel signal does
    // not leak into a fresh batch. Callers that want the
    // cancellation flag preserved across statements should use
    // their session cancellation-reset API explicitly between calls.
    if let Err(error) = context.runtime.cancellation.check() {
        return Err(abort_explicit_statement_error(
            context.transactions,
            error.into(),
        ));
    }
    if !context.persistent_backend && context.transactions.transaction_depth() == 0 {
        if let Some(plan) = context.cache.cached_optimized_sql_plan(sql) {
            let can_execute_without_transaction = match plan.as_ref() {
                uqa_sql::plan::UnifiedPlan::Query(query) => !query_requires_statement_transaction(
                    &context.effects.query_effect_context(),
                    query,
                )?,
                uqa_sql::plan::UnifiedPlan::Command(_) => false,
            };
            if can_execute_without_transaction {
                return UnifiedPlanExecutor::with_nested_statement(
                    context.statements.statement_execution_context(),
                    params,
                    nested_statement,
                )
                .with_source_sql(sql)
                .execute(plan.as_ref());
            }
        }
    }
    execute_uncached_or_snapshot_scoped(context, sql, params, nested_statement, consumer)
}

#[inline(never)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves statement transaction order"
)]
fn execute_uncached_or_snapshot_scoped<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    params: &[SQLParam],
    nested_statement: bool,
    consumer: &mut ResultConsumer<'_>,
) -> Result<SQLResult, SQLError> {
    // Parse an uncached batch completely before executing its first statement.
    // This preserves syntax atomicity. Exact single-statement cache hits reuse
    // the parsed AST and logical plan; batches still lower each statement only
    // when its turn arrives so earlier DDL, SET, ANALYZE, and function commands
    // can affect the following statement's semantics.
    let cached_statement = context.cache.cached_sql_statement(sql);
    let (statements, mut cached_entry) = match cached_statement {
        Some(cached) => (
            vec![StatementInput::Cached(cached.statement.clone())],
            Some(cached),
        ),
        None => match uqa_sql::parse_statements(sql) {
            Ok(statements) => (
                statements.into_iter().map(StatementInput::Parsed).collect(),
                None,
            ),
            Err(error) => return Err(abort_explicit_statement_error(context.transactions, error)),
        },
    };
    if statements.is_empty() {
        return Ok(SQLResult::empty());
    }
    let is_single_statement = statements.len() == 1;
    let final_statement_index = statements.len() - 1;
    let simple_query_batch = !is_single_statement;
    let mut implicit_segment_open = false;
    let execution = (|| -> Result<SQLResult, SQLError> {
        let mut last = SQLResult::empty();
        for (statement_index, statement) in statements.into_iter().enumerate() {
            if let Err(error) = context.runtime.cancellation.check() {
                return Err(abort_explicit_statement_error(
                    context.transactions,
                    error.into(),
                ));
            }
            let statement = statement
                .compile()
                .map_err(|error| abort_explicit_statement_error(context.transactions, error))?;
            let transaction = match &statement {
                uqa_sql::ast::Statement::Transaction(transaction) => Some(transaction.clone()),
                _ => None,
            };
            if simple_query_batch
                && transaction
                    .as_ref()
                    .is_some_and(transaction_requires_explicit_block)
                && (implicit_segment_open || context.transactions.transaction_depth() == 0)
            {
                return Err(no_active_transaction_error(
                    transaction.as_ref().expect("checked transaction command"),
                ));
            }
            if simple_query_batch
                && implicit_segment_open
                && transaction.as_ref().is_some_and(|transaction| {
                    matches!(
                        transaction,
                        uqa_sql::ast::TransactionStmt::Begin
                            | uqa_sql::ast::TransactionStmt::BeginWithCharacteristics(_)
                    )
                })
            {
                // PostgreSQL promotes the simple-query message's implicit
                // transaction to an explicit block. The preceding statements
                // stay uncommitted; a later COMMIT or ROLLBACK controls them.
                context.transactions.promote_simple_query_transaction()?;
                if let Some(uqa_sql::ast::TransactionStmt::BeginWithCharacteristics(options)) =
                    transaction
                {
                    context.transactions.run_transaction_statement(
                        uqa_sql::ast::TransactionStmt::SetCharacteristics(options),
                    )?;
                }
                implicit_segment_open = false;
                last = SQLResult::empty();
                last.command_tag = Some("BEGIN".into());
                if statement_index != final_statement_index {
                    consume_result(context, &last, consumer)?;
                }
                continue;
            }
            if simple_query_batch
                && transaction.is_none()
                && context.transactions.transaction_depth() == 0
                && !implicit_segment_open
            {
                context.transactions.begin_simple_query_transaction()?;
                implicit_segment_open = true;
            }
            let (initial_plan, cached_optimized_plan) = if is_single_statement {
                if let Some(cached) = cached_entry.take() {
                    (cached.logical_plan, cached.optimized_plan)
                } else {
                    let plan = Arc::new(UnifiedPlan::lower_with(
                        statement.clone(),
                        context.aggregates,
                    ));
                    context.cache.cache_sql_statement(
                        sql.to_string(),
                        Arc::new(statement.clone()),
                        Arc::clone(&plan),
                    );
                    (plan, None)
                }
            } else {
                (
                    Arc::new(UnifiedPlan::lower_with(
                        statement.clone(),
                        context.aggregates,
                    )),
                    None,
                )
            };
            if is_transaction_control(initial_plan.as_ref()) {
                // SQL COMMIT/ROLLBACK outside a block warn and succeed; the direct Rust transaction API keeps reporting misuse as an error.
                if context.transactions.transaction_depth() == 0
                    && transaction.as_ref().is_some_and(|transaction| {
                        matches!(
                            transaction,
                            uqa_sql::ast::TransactionStmt::Commit
                                | uqa_sql::ast::TransactionStmt::Rollback
                        )
                    })
                {
                    context.runtime.notices.lock().push((
                        "WARNING".into(),
                        "there is no transaction in progress".into(),
                    ));
                    last = SQLResult::empty();
                    last.command_tag = Some(
                        uqa_sql::result::completion::transaction_completion(
                            transaction.as_ref().expect("checked transaction command"),
                            false,
                        )
                        .into(),
                    );
                    if statement_index != final_statement_index {
                        consume_result(context, &last, consumer)?;
                    }
                    continue;
                }
                if simple_query_batch
                    && implicit_segment_open
                    && transaction.as_ref().is_some_and(|transaction| {
                        matches!(
                            transaction,
                            uqa_sql::ast::TransactionStmt::Commit
                                | uqa_sql::ast::TransactionStmt::Rollback
                        )
                    })
                {
                    context.runtime.notices.lock().push((
                        "WARNING".into(),
                        "there is no transaction in progress".into(),
                    ));
                }
                last = UnifiedPlanExecutor::with_nested_statement(
                    context.statements.statement_execution_context(),
                    params,
                    nested_statement || simple_query_batch,
                )
                .with_source_sql(sql)
                .execute(initial_plan.as_ref())?;
                if simple_query_batch
                    && transaction.as_ref().is_some_and(|transaction| {
                        matches!(
                            transaction,
                            uqa_sql::ast::TransactionStmt::Commit
                                | uqa_sql::ast::TransactionStmt::Rollback
                        )
                    })
                {
                    implicit_segment_open = false;
                }
                if statement_index != final_statement_index {
                    consume_result(context, &last, consumer)?;
                }
                continue;
            }
            let (has_row_locks, needs_row_lock_statement) = match initial_plan.as_ref() {
                uqa_sql::plan::UnifiedPlan::Query(query) => {
                    let has_row_locks = query_has_row_locks(query);
                    (has_row_locks, has_row_locks)
                }
                uqa_sql::plan::UnifiedPlan::Command(_) => (false, true),
            };
            let _row_lock_statement =
                needs_row_lock_statement.then(|| context.row_locks.begin_row_lock_statement());

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
                    .prepare_explicit_statement_snapshot(
                        uqa_sql::semantics::effects::read_only::plan_sets_transaction_snapshot(
                            initial_plan.as_ref(),
                        ),
                    )
                    .map_err(|error| context.transactions.abort_after_error(error))?;
                // The statement was lowered immediately above, after any earlier
                // BEGIN or catalog-changing statement in this batch completed.
                let mut plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
                if is_single_statement {
                    context.cache.cache_sql_statement(
                        sql.to_string(),
                        Arc::new(statement.clone()),
                        Arc::new(plan.clone()),
                    );
                }
                let mutating_query = match &plan {
                    uqa_sql::plan::UnifiedPlan::Query(query) => {
                        query_may_mutate_engine(&context.effects.query_effect_context(), query)
                    }
                    uqa_sql::plan::UnifiedPlan::Command(_) => Ok(false),
                }
                .map_err(|error| context.transactions.abort_after_error(error))?;
                if mutating_query
                    && !has_row_locks
                    && context
                        .transactions
                        .prepare_explicit_transaction_writer()
                        .map_err(|error| context.transactions.abort_after_error(error))?
                {
                    plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
                    if is_single_statement {
                        context.cache.cache_sql_statement(
                            sql.to_string(),
                            Arc::new(statement.clone()),
                            Arc::new(plan.clone()),
                        );
                    }
                }
                let optimized = match context.planning.plan_for_execution(plan, params) {
                    Ok(plan) => plan,
                    Err(error) => {
                        return Err(context.transactions.abort_after_error(error));
                    }
                };
                let mut executor = UnifiedPlanExecutor::with_nested_statement(
                    context.statements.statement_execution_context(),
                    params,
                    nested_statement || simple_query_batch,
                )
                .with_source_sql(sql);
                match executor.execute(&optimized) {
                    Ok(result) => last = result,
                    Err(error) => return Err(context.transactions.abort_after_error(error)),
                }
                if statement_index != final_statement_index {
                    consume_result(context, &last, consumer)?;
                }
                continue;
            }

            // Every persistent SQL statement owns one storage transaction when
            // the caller has not opened an explicit transaction. This is the
            // actual autocommit boundary: catalog, document, FTS, btree, vector,
            // graph, and registry writes either commit together or are all rolled
            // back. Memory commands use the same boundary so a fallible multi-row
            // mutation restores its pre-statement snapshot; read-only memory
            // queries avoid copying the whole database.
            let (is_read_query, requires_statement_transaction) = match initial_plan.as_ref() {
                uqa_sql::plan::UnifiedPlan::Query(query) => {
                    let mutates =
                        query_may_mutate_engine(&context.effects.query_effect_context(), query)?;
                    (
                        !mutates,
                        mutates
                            || query_requires_statement_transaction(
                                &context.effects.query_effect_context(),
                                query,
                            )?,
                    )
                }
                uqa_sql::plan::UnifiedPlan::Command(_) => (false, true),
            };
            let runs_outside_transaction = matches!(
                initial_plan.as_ref(),
                uqa_sql::plan::UnifiedPlan::Command(command)
                    if matches!(
                        command.as_ref(),
                        uqa_sql::plan::CommandPlan::Discard { .. }
                            | uqa_sql::plan::CommandPlan::Vacuum(_)
                    )
            );
            let needs_implicit_transaction = !runs_outside_transaction
                && (context.persistent_backend || requires_statement_transaction || has_row_locks);
            if needs_implicit_transaction {
                if has_row_locks {
                    context.row_locks.statement_row_lock_cache()?;
                }
                context
                    .transactions
                    .begin_implicit_statement_transaction(is_read_query)?;
                // Catalog/table refresh intentionally invalidates cached logical
                // plans even when the in-process generation did not move: a
                // sibling SQLite writer can release its lock immediately before
                // publishing that generation. Re-lower the parsed statement while
                // the database snapshot is pinned, then optimize that exact plan.
                let mut plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
                if is_single_statement {
                    context.cache.cache_sql_statement(
                        sql.to_string(),
                        Arc::new(statement.clone()),
                        Arc::new(plan.clone()),
                    );
                }
                let must_restart_as_writer =
                    if is_read_query && context.persistent_backend && !has_row_locks {
                        match &plan {
                            uqa_sql::plan::UnifiedPlan::Query(query) => {
                                match query_may_mutate_engine(
                                    &context.effects.query_effect_context(),
                                    query,
                                ) {
                                    Ok(mutates) => mutates,
                                    Err(error) => {
                                        return rollback_after_statement_error(
                                            context.transactions,
                                            error,
                                        )
                                    }
                                }
                            }
                            uqa_sql::plan::UnifiedPlan::Command(_) => false,
                        }
                    } else {
                        false
                    };
                if must_restart_as_writer {
                    rollback_implicit_statement(
                        context.transactions,
                        "restart read transaction as writer",
                    )?;
                    context
                        .transactions
                        .begin_implicit_statement_transaction(false)?;
                    plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
                    if is_single_statement {
                        context.cache.cache_sql_statement(
                            sql.to_string(),
                            Arc::new(statement.clone()),
                            Arc::new(plan.clone()),
                        );
                    }
                }
                let mutating_query = match &plan {
                    uqa_sql::plan::UnifiedPlan::Query(query) => {
                        query_may_mutate_engine(&context.effects.query_effect_context(), query)
                    }
                    uqa_sql::plan::UnifiedPlan::Command(_) => Ok(false),
                };
                let mutating_query = match mutating_query {
                    Ok(mutates) => mutates,
                    Err(error) => {
                        return rollback_after_statement_error(context.transactions, error)
                    }
                };
                if mutating_query && context.persistent_backend && !has_row_locks {
                    match context.transactions.prepare_explicit_transaction_writer() {
                        Ok(true) => {
                            plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
                            if is_single_statement {
                                context.cache.cache_sql_statement(
                                    sql.to_string(),
                                    Arc::new(statement.clone()),
                                    Arc::new(plan.clone()),
                                );
                            }
                        }
                        Ok(false) => {}
                        Err(error) => {
                            return rollback_after_statement_error(context.transactions, error)
                        }
                    }
                }
                let optimized = match context.planning.plan_for_execution(plan, params) {
                    Ok(plan) => plan,
                    Err(error) => {
                        return rollback_after_statement_error(context.transactions, error)
                    }
                };
                let mut executor = UnifiedPlanExecutor::with_nested_statement(
                    context.statements.statement_execution_context(),
                    params,
                    nested_statement || simple_query_batch,
                )
                .with_source_sql(sql);
                match executor.execute(&optimized) {
                    Ok(result) => {
                        // Commit failure cleanup is owned by the transaction
                        // layer, including cache restoration and stack reset.
                        context
                            .transactions
                            .run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit)?;
                        last = result;
                    }
                    Err(statement_error) => {
                        return rollback_after_statement_error(
                            context.transactions,
                            statement_error,
                        )
                    }
                }
            } else {
                // In-memory read-only queries run without a transaction snapshot.
                // Their cache generation is invalidated by every table, catalog,
                // search-path, and function-registry change, so both parsing and
                // physical optimization are reusable until that generation moves.
                let optimized = if let Some(plan) = cached_optimized_plan {
                    plan
                } else {
                    let plan = Arc::new(
                        context
                            .planning
                            .plan_for_execution(initial_plan.as_ref().clone(), params)?,
                    );
                    if is_single_statement {
                        context
                            .cache
                            .cache_optimized_sql_plan(sql, Arc::clone(&plan));
                    }
                    plan
                };
                last = UnifiedPlanExecutor::with_nested_statement(
                    context.statements.statement_execution_context(),
                    params,
                    nested_statement || simple_query_batch,
                )
                .with_source_sql(sql)
                .execute(optimized.as_ref())?;
            }
            if statement_index != final_statement_index {
                consume_result(context, &last, consumer)?;
            }
        }
        Ok(last)
    })();
    if !simple_query_batch {
        return execution;
    }
    match execution {
        Ok(result) if implicit_segment_open => {
            context
                .transactions
                .run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit)?;
            Ok(result)
        }
        Ok(result) => Ok(result),
        Err(error) if implicit_segment_open && context.transactions.transaction_depth() != 0 => {
            rollback_after_statement_error(context.transactions, error)
        }
        Err(error) => Err(error),
    }
}
