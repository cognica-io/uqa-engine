//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL batch scheduling over live cache, transaction and statement inputs.

use super::{
    plan_executor::{StatementOutput, UnifiedPlanExecutor},
    transactions::{
        abort_explicit_statement_error, rollback_after_statement_error, rollback_implicit_statement,
    },
};
use crate::query::locking::query_has_row_locks;
use context::{BatchExecutionContext, CachedStatement};
use std::sync::Arc;
use uqa_sql::{
    plan::UnifiedPlan,
    semantics::effects::{
        is_transaction_control, query_may_mutate_engine, query_requires_statement_transaction,
        transaction_blocks::{no_active_transaction_error, transaction_requires_explicit_block},
    },
    SQLError, SQLParam, SQLResult,
};

mod analysis;
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

/// Execute native syntax through the same snapshot, deadline and transaction scheduler as SQL text, without reading or publishing text-cache entries.
pub fn execute_prepared_statement<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    statement: Arc<uqa_sql::Statement>,
    name: &str,
    sql: Option<&str>,
    params: &[SQLParam],
    nested_statement: bool,
) -> Result<SQLResult, SQLError> {
    context
        .runtime
        .cancellation
        .check()
        .map_err(|error| abort_explicit_statement_error(context.transactions, error.into()))?;
    let _statement_thread =
        super::context::statement_thread_scope(context.statements, context.runtime.cancellation);
    let display = context
        .statements
        .diagnostic_search_path()
        .map(|path| uqa_sql::ast::TypeDisplayScope::enter(&path));
    let logical_plan = Arc::new(UnifiedPlan::lower_with(
        statement.as_ref().clone(),
        context.aggregates,
    ));
    execute_uncached_or_snapshot_scoped(
        context,
        StatementSource {
            sql,
            prepared: Some(name),
        },
        params,
        nested_statement,
        &mut None,
        display.as_ref(),
        Some(CachedStatement {
            statement,
            logical_plan,
            optimized_plan: None,
            parser: uqa_sql::parser::ParserMetadata::default(),
        }),
    )
}

pub fn execute_nested<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    execute_with_context(context, sql, params, true, &mut None)
}

enum ResultConsumer<'a> {
    Completed(&'a mut dyn FnMut(&SQLResult) -> Result<(), SQLError>),
    WithinStatement(&'a mut dyn FnMut(StatementOutput) -> Result<(), SQLError>),
}

/// Consume results before the implicit statement or batch commits. Consumer errors
/// follow the same rollback/abort path as errors raised by statement execution.
pub fn execute_with_output<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    params: &[SQLParam],
    consume: &mut dyn FnMut(StatementOutput) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    execute_with_context(
        context,
        sql,
        params,
        false,
        &mut Some(ResultConsumer::WithinStatement(consume)),
    )
    .map(|_| ())
}

fn execute_plan<S: Clone + Send + Sync + 'static>(
    executor: &mut UnifiedPlanExecutor<'_, '_, S>,
    plan: &UnifiedPlan,
    consumer: &mut Option<ResultConsumer<'_>>,
    prepared: Option<&str>,
) -> Result<SQLResult, SQLError> {
    if let Some(name) = prepared {
        return executor.execute_host_prepared(name);
    }
    if let Some(ResultConsumer::WithinStatement(consume)) = consumer {
        consume(executor.execute_for_output(plan)?)?;
        // This entry point hands off the result and returns no public row vector.
        Ok(SQLResult::empty())
    } else {
        executor.execute(plan)
    }
}

enum StatementInput<'sql> {
    Cached(Arc<uqa_sql::Statement>),
    Parsed(uqa_sql::ParsedStatement<'sql>),
}

impl StatementInput<'_> {
    fn is_notification_listener_command(&self) -> bool {
        match self {
            Self::Cached(statement) => matches!(
                statement.as_ref(),
                uqa_sql::Statement::Listen { .. } | uqa_sql::Statement::Unlisten { .. }
            ),
            Self::Parsed(statement) => statement.is_notification_listener_command(),
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
    let mut consumer = Some(ResultConsumer::Completed(consume));
    let result = execute_with_context(context, sql, params, nested_statement, &mut consumer)?;
    consume_result(context, &result, &mut consumer)
}

fn consume_result<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    result: &SQLResult,
    consumer: &mut Option<ResultConsumer<'_>>,
) -> Result<(), SQLError> {
    if let Some(ResultConsumer::Completed(consume)) = consumer {
        let _statement_thread = super::context::statement_thread_scope(
            context.statements,
            context.runtime.cancellation,
        );
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
    consumer: &mut Option<ResultConsumer<'_>>,
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
    let _statement_thread =
        super::context::statement_thread_scope(context.statements, context.runtime.cancellation);
    // Diagnostics spell user-defined types as the statement's search path finds them.
    let display = context
        .statements
        .diagnostic_search_path()
        .map(|path| uqa_sql::ast::TypeDisplayScope::enter(&path));
    let cached = context.cache.cached_sql_statement(sql);
    if !context.persistent_backend && context.transactions.transaction_depth() == 0 {
        if let Some((plan, parser)) = cached.as_ref().and_then(|cached| {
            cached
                .optimized_plan
                .as_ref()
                .map(|plan| (plan, &cached.parser))
        }) {
            let _statement_deadline = (!nested_statement)
                .then(|| context::statement_deadline(context))
                .flatten();
            let can_execute_without_transaction = match plan.as_ref() {
                uqa_sql::plan::UnifiedPlan::Query(query) => !query_requires_statement_transaction(
                    &context.effects.query_effect_context(),
                    query,
                )?,
                uqa_sql::plan::UnifiedPlan::Command(_) => false,
            };
            if can_execute_without_transaction {
                for notice in parser.notices.iter() {
                    context.runtime.notices.push(notice.clone());
                }
                let mut executor = UnifiedPlanExecutor::with_nested_statement(
                    context.statements.statement_execution_context(),
                    params,
                    nested_statement,
                )
                .with_source_sql(sql);
                return execute_plan(&mut executor, plan.as_ref(), consumer, None);
            }
        }
    }
    execute_uncached_or_snapshot_scoped(
        context,
        StatementSource {
            sql: Some(sql),
            prepared: None,
        },
        params,
        nested_statement,
        consumer,
        display.as_ref(),
        cached,
    )
}

#[derive(Clone, Copy)]
struct StatementSource<'a> {
    sql: Option<&'a str>,
    prepared: Option<&'a str>,
}

#[inline(never)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves statement transaction order"
)]
fn execute_uncached_or_snapshot_scoped<S: Clone + Send + Sync + 'static>(
    context: &BatchExecutionContext<'_, S>,
    source: StatementSource<'_>,
    params: &[SQLParam],
    nested_statement: bool,
    consumer: &mut Option<ResultConsumer<'_>>,
    display: Option<&uqa_sql::ast::TypeDisplayScope>,
    cached_statement: Option<CachedStatement>,
) -> Result<SQLResult, SQLError> {
    let source_sql = source.sql;
    let sql = source_sql.unwrap_or_default();
    // Parse an uncached batch completely before executing its first statement.
    // This preserves syntax atomicity. Exact single-statement cache hits reuse
    // the parsed AST and logical plan; batches still lower each statement only
    // when its turn arrives so earlier DDL, SET, ANALYZE, and function commands
    // can affect the following statement's semantics.
    let (statements, mut cached_entry, parser) = match cached_statement {
        Some(cached) => {
            let parser = cached.parser.clone();
            for notice in parser.notices.iter() {
                context.runtime.notices.push(notice.clone());
            }
            (
                vec![StatementInput::Cached(cached.statement.clone())],
                Some(cached),
                parser,
            )
        }
        None => {
            let (statements, parser) =
                uqa_sql::parser::with_settings(context.statements.parser_settings(), || {
                    uqa_sql::parse_statements(sql)
                });
            for notice in parser.notices.iter() {
                context.runtime.notices.push(notice.clone());
            }
            let statements = statements
                .map_err(|error| abort_explicit_statement_error(context.transactions, error))?;
            (
                statements.into_iter().map(StatementInput::Parsed).collect(),
                None,
                parser,
            )
        }
    };
    if context.statements.notification_subscriptions_required() {
        for statement in &statements {
            context.runtime.cancellation.check().map_err(|error| {
                abort_explicit_statement_error(context.transactions, error.into())
            })?;
            if statement.is_notification_listener_command() {
                return Err(abort_explicit_statement_error(
                    context.transactions,
                    SQLError::NotificationRequiresSubscription,
                ));
            }
        }
    }
    if statements.is_empty() {
        return Ok(SQLResult::empty());
    }
    let is_single_statement = statements.len() == 1;
    let cache_single_statement =
        is_single_statement && source_sql.is_some() && source.prepared.is_none();
    let final_statement_index = statements.len() - 1;
    let simple_query_batch = !is_single_statement;
    let mut implicit_segment_open = false;
    let execution = (|| -> Result<SQLResult, SQLError> {
        let mut last = SQLResult::empty();
        for (statement_index, statement) in statements.into_iter().enumerate() {
            let _statement_deadline = (!nested_statement)
                .then(|| context::statement_deadline(context))
                .flatten();
            if let Err(error) = context.runtime.cancellation.check() {
                return Err(abort_explicit_statement_error(
                    context.transactions,
                    error.into(),
                ));
            }
            // An earlier command can have completed or chained the transaction while the message clock stays fixed.
            let _statement_thread = super::context::statement_thread_scope(
                context.statements,
                context.runtime.cancellation,
            );
            // An earlier statement of the message may have changed the search path.
            if let (Some(display), Some(path)) = (
                display.filter(|_| statement_index > 0),
                context.statements.diagnostic_search_path(),
            ) {
                display.refresh(&path);
            }
            let (parsed_statement, batch_statement) = match statement {
                StatementInput::Cached(statement) => (Some(statement), None),
                StatementInput::Parsed(statement) => {
                    let statement = statement.compile().map_err(|error| {
                        abort_explicit_statement_error(context.transactions, error)
                    })?;
                    if is_single_statement {
                        (Some(Arc::new(statement)), None)
                    } else {
                        (None, Some(statement))
                    }
                }
            };
            let statement = parsed_statement
                .as_deref()
                .or(batch_statement.as_ref())
                .expect("compiled statement");
            let shared_statement =
                || Arc::clone(parsed_statement.as_ref().expect("single statement cache"));
            let transaction = match statement {
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
                        shared_statement(),
                        Arc::clone(&plan),
                        parser.clone(),
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
                // Text commands have no retained inputs to bind. Prepared commands reach the same completion after their native argument conversion.
                if source.prepared.is_none() {
                    if let Some(completed) = transaction.as_ref().and_then(|statement| {
                        super::transactions::complete_idle_transaction(
                            context.transactions,
                            context.runtime.notices,
                            statement,
                        )
                    }) {
                        last = completed;
                        if statement_index != final_statement_index {
                            consume_result(context, &last, consumer)?;
                        }
                        continue;
                    }
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
                    context.runtime.notices.push(
                        uqa_sql::semantics::effects::transaction_blocks::no_transaction_in_progress_warning(),
                    );
                }
                let mut executor = UnifiedPlanExecutor::with_nested_statement(
                    context.statements.statement_execution_context(),
                    params,
                    nested_statement || simple_query_batch,
                )
                .with_optional_source_sql(source_sql);
                last = execute_plan(
                    &mut executor,
                    initial_plan.as_ref(),
                    consumer,
                    source.prepared,
                )?;
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
            let empty_statement = matches!(
                initial_plan.as_ref(),
                UnifiedPlan::Command(command) if matches!(command.as_ref(), uqa_sql::plan::CommandPlan::Empty)
            );
            let (has_row_locks, needs_row_lock_statement) = match initial_plan.as_ref() {
                uqa_sql::plan::UnifiedPlan::Query(query) => {
                    let has_row_locks = query_has_row_locks(query);
                    (has_row_locks, has_row_locks)
                }
                uqa_sql::plan::UnifiedPlan::Command(_) => (false, !empty_statement),
            };
            let _row_lock_statement =
                needs_row_lock_statement.then(|| context.row_locks.begin_row_lock_statement());

            if context.transactions.transaction_depth() != 0 {
                last = context::with_active_transaction(context.transactions, || {
                    if has_row_locks {
                        context.row_locks.statement_row_lock_cache()?;
                    }
                    if !empty_statement {
                        context.transactions.prepare_explicit_statement_snapshot(
                            uqa_sql::semantics::effects::read_only::plan_sets_transaction_snapshot(
                                initial_plan.as_ref(),
                            ),
                        )?;
                    }
                    // Earlier BEGIN or catalog changes precede this statement's admitted snapshot.
                    let mut plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
                    if cache_single_statement {
                        context.cache.cache_sql_statement(
                            sql.to_string(),
                            shared_statement(),
                            Arc::new(plan.clone()),
                            parser.clone(),
                        );
                    }
                    let mutating_query = match &plan {
                        uqa_sql::plan::UnifiedPlan::Query(query) => {
                            query_may_mutate_engine(&context.effects.query_effect_context(), query)
                        }
                        uqa_sql::plan::UnifiedPlan::Command(_) => Ok(false),
                    }?;
                    if mutating_query
                        && !has_row_locks
                        && context.transactions.prepare_explicit_transaction_writer()?
                    {
                        plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
                        if cache_single_statement {
                            context.cache.cache_sql_statement(
                                sql.to_string(),
                                shared_statement(),
                                Arc::new(plan.clone()),
                                parser.clone(),
                            );
                        }
                    }
                    let optimized = analysis::plan(
                        context,
                        sql,
                        plan,
                        params,
                        cache_single_statement && !nested_statement,
                        source.prepared.is_some(),
                    )?;
                    let mut executor = UnifiedPlanExecutor::with_nested_statement(
                        context.statements.statement_execution_context(),
                        params,
                        nested_statement || simple_query_batch,
                    )
                    .with_optional_source_sql(source_sql);
                    execute_plan(&mut executor, &optimized, consumer, source.prepared)
                })?;
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
                        uqa_sql::plan::CommandPlan::Empty
                            | uqa_sql::plan::CommandPlan::Discard { .. }
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
                if let Err(error) = context.transactions.prepare_explicit_statement_snapshot(
                    uqa_sql::semantics::effects::read_only::plan_sets_transaction_snapshot(
                        initial_plan.as_ref(),
                    ),
                ) {
                    return rollback_after_statement_error(context.transactions, error);
                }
                // Catalog/table refresh intentionally invalidates cached logical
                // plans even when the in-process generation did not move: a
                // sibling SQLite writer can release its lock immediately before
                // publishing that generation. Re-lower the parsed statement while
                // the database snapshot is pinned, then optimize that exact plan.
                let mut plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
                if cache_single_statement {
                    context.cache.cache_sql_statement(
                        sql.to_string(),
                        shared_statement(),
                        Arc::new(plan.clone()),
                        parser.clone(),
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
                    if let Err(error) = context.transactions.prepare_explicit_statement_snapshot(
                        uqa_sql::semantics::effects::read_only::plan_sets_transaction_snapshot(
                            initial_plan.as_ref(),
                        ),
                    ) {
                        return rollback_after_statement_error(context.transactions, error);
                    }
                    plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
                    if cache_single_statement {
                        context.cache.cache_sql_statement(
                            sql.to_string(),
                            shared_statement(),
                            Arc::new(plan.clone()),
                            parser.clone(),
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
                            if cache_single_statement {
                                context.cache.cache_sql_statement(
                                    sql.to_string(),
                                    shared_statement(),
                                    Arc::new(plan.clone()),
                                    parser.clone(),
                                );
                            }
                        }
                        Ok(false) => {}
                        Err(error) => {
                            return rollback_after_statement_error(context.transactions, error)
                        }
                    }
                }
                let optimized = match analysis::plan(
                    context,
                    sql,
                    plan,
                    params,
                    cache_single_statement && !nested_statement,
                    source.prepared.is_some(),
                ) {
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
                .with_optional_source_sql(source_sql);
                match execute_plan(&mut executor, &optimized, consumer, source.prepared) {
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
                // Catalog generations protect identity, while SQL analysis decides
                // whether input constants also require a fresh message boundary.
                let optimized = if source.prepared.is_some() {
                    initial_plan.clone()
                } else if let Some(plan) = cached_optimized_plan {
                    plan
                } else {
                    let (plan, reusable) = context
                        .planning
                        .plan_for_statement_cache(initial_plan.as_ref().clone(), params)?;
                    let plan = Arc::new(plan);
                    if cache_single_statement && reusable {
                        context
                            .cache
                            .cache_optimized_sql_plan(sql, Arc::clone(&plan));
                    }
                    plan
                };
                let mut executor = UnifiedPlanExecutor::with_nested_statement(
                    context.statements.statement_execution_context(),
                    params,
                    nested_statement || simple_query_batch,
                )
                .with_optional_source_sql(source_sql);
                last = execute_plan(&mut executor, optimized.as_ref(), consumer, source.prepared)?;
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
    let _statement_thread =
        super::context::statement_thread_scope(context.statements, context.runtime.cancellation);
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
