//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    compile, is_transaction_control, lower_statement, optimize_engine_plan, query_has_row_locks,
    query_may_mutate_engine, query_requires_statement_transaction, Arc, Engine, SQLError, SQLParam,
    SQLResult, UnifiedPlanExecutor,
};

pub(crate) fn execute(
    engine: &Engine,
    sql: &str,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    execute_with_context(engine, sql, params, false)
}

pub(crate) fn execute_nested(
    engine: &Engine,
    sql: &str,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    execute_with_context(engine, sql, params, true)
}

fn execute_with_context(
    engine: &Engine,
    sql: &str,
    params: &[SQLParam],
    nested_statement: bool,
) -> Result<SQLResult, SQLError> {
    // Reject cancelled tokens up-front so a stale cancel signal does
    // not leak into a fresh batch. Callers that want the
    // cancellation flag preserved across statements should use
    // [`crate::Engine::reset_cancellation`] explicitly between calls.
    if let Err(error) = engine.cancellation_token().check() {
        return Err(abort_explicit_statement_error(engine, error.into()));
    }
    if engine.storage.backend.is_none() && engine.transaction_depth() == 0 {
        if let Some(plan) = engine.cached_optimized_sql_plan(sql) {
            let can_execute_without_transaction = match plan.as_ref() {
                uqa_planner::UnifiedPlan::Query(query) => {
                    !query_requires_statement_transaction(engine, query)?
                }
                uqa_planner::UnifiedPlan::Command(_) => false,
            };
            if can_execute_without_transaction {
                return UnifiedPlanExecutor::with_nested_statement(
                    engine,
                    params,
                    nested_statement,
                )
                .execute(plan.as_ref());
            }
        }
    }
    execute_uncached_or_snapshot_scoped(engine, sql, params, nested_statement)
}

#[inline(never)]
fn execute_uncached_or_snapshot_scoped(
    engine: &Engine,
    sql: &str,
    params: &[SQLParam],
    nested_statement: bool,
) -> Result<SQLResult, SQLError> {
    // Parse an uncached batch completely before executing its first statement.
    // This preserves syntax atomicity. Exact single-statement cache hits reuse
    // the parsed AST and logical plan; batches still lower each statement only
    // when its turn arrives so earlier DDL, SET, ANALYZE, and function commands
    // can affect the following statement's semantics.
    let cached_statement = engine.cached_sql_statement(sql);
    let (statements, mut cached_entry) = match cached_statement {
        Some(cached) => (vec![cached.statement.as_ref().clone()], Some(cached)),
        None => match compile(sql) {
            Ok(statements) => (statements, None),
            Err(error) => return Err(abort_explicit_statement_error(engine, error)),
        },
    };
    if statements.is_empty() {
        return Ok(SQLResult::empty());
    }
    let is_single_statement = statements.len() == 1;
    let simple_query_batch = !is_single_statement;
    let mut implicit_segment_open = false;
    let execution = (|| -> Result<SQLResult, SQLError> {
        let mut last = SQLResult::empty();
        for statement in statements {
            if let Err(error) = engine.cancellation_token().check() {
                return Err(abort_explicit_statement_error(engine, error.into()));
            }
            let transaction = match &statement {
                uqa_sql::ast::Statement::Transaction(transaction) => Some(transaction.clone()),
                _ => None,
            };
            if simple_query_batch
                && transaction
                    .as_ref()
                    .is_some_and(transaction_requires_explicit_block)
                && (implicit_segment_open || engine.transaction_depth() == 0)
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
                engine.promote_implicit_transaction_block()?;
                if let Some(uqa_sql::ast::TransactionStmt::BeginWithCharacteristics(options)) =
                    transaction
                {
                    engine.run_transaction_statement(
                        uqa_sql::ast::TransactionStmt::SetCharacteristics(options),
                    )?;
                }
                implicit_segment_open = false;
                last = SQLResult::empty();
                continue;
            }
            if simple_query_batch
                && transaction.is_none()
                && engine.transaction_depth() == 0
                && !implicit_segment_open
            {
                engine.begin_implicit_transaction_block()?;
                implicit_segment_open = true;
            }
            let (initial_plan, cached_optimized_plan) = if is_single_statement {
                if let Some(cached) = cached_entry.take() {
                    (cached.logical_plan, cached.optimized_plan)
                } else {
                    let plan = Arc::new(lower_statement(engine, statement.clone()));
                    engine.cache_sql_statement(
                        sql.to_string(),
                        Arc::new(statement.clone()),
                        Arc::clone(&plan),
                    );
                    (plan, None)
                }
            } else {
                (Arc::new(lower_statement(engine, statement.clone())), None)
            };
            if is_transaction_control(initial_plan.as_ref()) {
                // SQL COMMIT/ROLLBACK outside a block warn and succeed; the direct Rust transaction API keeps reporting misuse as an error.
                if engine.transaction_depth() == 0
                    && transaction.as_ref().is_some_and(|transaction| {
                        matches!(
                            transaction,
                            uqa_sql::ast::TransactionStmt::Commit
                                | uqa_sql::ast::TransactionStmt::Rollback
                        )
                    })
                {
                    engine.push_sql_notice("WARNING", "there is no transaction in progress");
                    last = SQLResult::empty();
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
                    engine.push_sql_notice("WARNING", "there is no transaction in progress");
                }
                last = UnifiedPlanExecutor::with_nested_statement(
                    engine,
                    params,
                    nested_statement || simple_query_batch,
                )
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
                continue;
            }
            let (has_row_locks, needs_row_lock_statement) = match initial_plan.as_ref() {
                uqa_planner::UnifiedPlan::Query(query) => {
                    let has_row_locks = query_has_row_locks(query);
                    (has_row_locks, has_row_locks)
                }
                uqa_planner::UnifiedPlan::Command(_) => (false, true),
            };
            let _row_lock_statement =
                needs_row_lock_statement.then(|| engine.begin_row_lock_statement());

            if engine.transaction_depth() != 0 {
                engine.ensure_transaction_usable()?;
                if has_row_locks {
                    engine
                        .statement_row_lock_cache()
                        .map_err(|error| engine.abort_sql_transaction_after_error(error))?;
                }
                engine
                    .prepare_explicit_statement_snapshot(
                        super::read_only::plan_sets_transaction_snapshot(initial_plan.as_ref()),
                    )
                    .map_err(|error| engine.abort_sql_transaction_after_error(error))?;
                // The statement was lowered immediately above, after any earlier
                // BEGIN or catalog-changing statement in this batch completed.
                let mut plan = lower_statement(engine, statement.clone());
                if is_single_statement {
                    engine.cache_sql_statement(
                        sql.to_string(),
                        Arc::new(statement.clone()),
                        Arc::new(plan.clone()),
                    );
                }
                let mutating_query = match &plan {
                    uqa_planner::UnifiedPlan::Query(query) => {
                        query_may_mutate_engine(engine, query)
                    }
                    uqa_planner::UnifiedPlan::Command(_) => Ok(false),
                }
                .map_err(|error| engine.abort_sql_transaction_after_error(error))?;
                if mutating_query
                    && !has_row_locks
                    && engine
                        .prepare_explicit_transaction_writer()
                        .map_err(|error| engine.abort_sql_transaction_after_error(error))?
                {
                    plan = lower_statement(engine, statement.clone());
                    if is_single_statement {
                        engine.cache_sql_statement(
                            sql.to_string(),
                            Arc::new(statement.clone()),
                            Arc::new(plan.clone()),
                        );
                    }
                }
                let optimized = match optimize_engine_plan(engine, plan) {
                    Ok(plan) => plan,
                    Err(error) => {
                        return Err(engine.abort_sql_transaction_after_error(error));
                    }
                };
                let mut executor = UnifiedPlanExecutor::with_nested_statement(
                    engine,
                    params,
                    nested_statement || simple_query_batch,
                );
                match executor.execute(&optimized) {
                    Ok(result) => last = result,
                    Err(error) => return Err(engine.abort_sql_transaction_after_error(error)),
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
                uqa_planner::UnifiedPlan::Query(query) => {
                    let mutates = query_may_mutate_engine(engine, query)?;
                    (
                        !mutates,
                        mutates || query_requires_statement_transaction(engine, query)?,
                    )
                }
                uqa_planner::UnifiedPlan::Command(_) => (false, true),
            };
            let runs_outside_transaction = matches!(
                initial_plan.as_ref(),
                uqa_planner::UnifiedPlan::Command(command)
                    if matches!(
                        command.as_ref(),
                        uqa_planner::CommandPlan::Discard { .. }
                            | uqa_planner::CommandPlan::Vacuum(_)
                    )
            );
            let needs_implicit_transaction = !runs_outside_transaction
                && (engine.storage.backend.is_some()
                    || requires_statement_transaction
                    || has_row_locks);
            if needs_implicit_transaction {
                if has_row_locks {
                    engine.statement_row_lock_cache()?;
                }
                engine.begin_implicit_statement_transaction(is_read_query)?;
                // Catalog/table refresh intentionally invalidates cached logical
                // plans even when the in-process generation did not move: a
                // sibling SQLite writer can release its lock immediately before
                // publishing that generation. Re-lower the parsed statement while
                // the database snapshot is pinned, then optimize that exact plan.
                let mut plan = lower_statement(engine, statement.clone());
                if is_single_statement {
                    engine.cache_sql_statement(
                        sql.to_string(),
                        Arc::new(statement.clone()),
                        Arc::new(plan.clone()),
                    );
                }
                let must_restart_as_writer = if is_read_query
                    && engine.storage.backend.is_some()
                    && !has_row_locks
                {
                    match &plan {
                        uqa_planner::UnifiedPlan::Query(query) => {
                            match query_may_mutate_engine(engine, query) {
                                Ok(mutates) => mutates,
                                Err(error) => return rollback_after_statement_error(engine, error),
                            }
                        }
                        uqa_planner::UnifiedPlan::Command(_) => false,
                    }
                } else {
                    false
                };
                if must_restart_as_writer {
                    rollback_implicit_statement(engine, "restart read transaction as writer")?;
                    engine.begin_implicit_statement_transaction(false)?;
                    plan = lower_statement(engine, statement.clone());
                    if is_single_statement {
                        engine.cache_sql_statement(
                            sql.to_string(),
                            Arc::new(statement.clone()),
                            Arc::new(plan.clone()),
                        );
                    }
                }
                let mutating_query = match &plan {
                    uqa_planner::UnifiedPlan::Query(query) => {
                        query_may_mutate_engine(engine, query)
                    }
                    uqa_planner::UnifiedPlan::Command(_) => Ok(false),
                };
                let mutating_query = match mutating_query {
                    Ok(mutates) => mutates,
                    Err(error) => return rollback_after_statement_error(engine, error),
                };
                if mutating_query && engine.storage.backend.is_some() && !has_row_locks {
                    match engine.prepare_explicit_transaction_writer() {
                        Ok(true) => {
                            plan = lower_statement(engine, statement.clone());
                            if is_single_statement {
                                engine.cache_sql_statement(
                                    sql.to_string(),
                                    Arc::new(statement.clone()),
                                    Arc::new(plan.clone()),
                                );
                            }
                        }
                        Ok(false) => {}
                        Err(error) => return rollback_after_statement_error(engine, error),
                    }
                }
                let optimized = match optimize_engine_plan(engine, plan) {
                    Ok(plan) => plan,
                    Err(error) => return rollback_after_statement_error(engine, error),
                };
                let mut executor = UnifiedPlanExecutor::with_nested_statement(
                    engine,
                    params,
                    nested_statement || simple_query_batch,
                );
                match executor.execute(&optimized) {
                    Ok(result) => {
                        // Commit failure cleanup is owned by the transaction
                        // layer, including cache restoration and stack reset.
                        engine.run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit)?;
                        last = result;
                    }
                    Err(statement_error) => {
                        return rollback_after_statement_error(engine, statement_error)
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
                    let plan =
                        Arc::new(optimize_engine_plan(engine, initial_plan.as_ref().clone())?);
                    if is_single_statement {
                        engine.cache_optimized_sql_plan(sql, Arc::clone(&plan));
                    }
                    plan
                };
                last = UnifiedPlanExecutor::with_nested_statement(
                    engine,
                    params,
                    nested_statement || simple_query_batch,
                )
                .execute(optimized.as_ref())?;
            }
        }
        Ok(last)
    })();
    if !simple_query_batch {
        return execution;
    }
    match execution {
        Ok(result) if implicit_segment_open => {
            engine.run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit)?;
            Ok(result)
        }
        Ok(result) => Ok(result),
        Err(error) if implicit_segment_open && engine.transaction_depth() != 0 => {
            rollback_after_statement_error(engine, error)
        }
        Err(error) => Err(error),
    }
}

fn transaction_requires_explicit_block(transaction: &uqa_sql::ast::TransactionStmt) -> bool {
    matches!(
        transaction,
        uqa_sql::ast::TransactionStmt::Savepoint(_)
            | uqa_sql::ast::TransactionStmt::ReleaseSavepoint(_)
            | uqa_sql::ast::TransactionStmt::RollbackToSavepoint(_)
            | uqa_sql::ast::TransactionStmt::CommitAndChain
            | uqa_sql::ast::TransactionStmt::RollbackAndChain
    )
}

fn no_active_transaction_error(transaction: &uqa_sql::ast::TransactionStmt) -> SQLError {
    let command = match transaction {
        uqa_sql::ast::TransactionStmt::Savepoint(_) => "SAVEPOINT",
        uqa_sql::ast::TransactionStmt::ReleaseSavepoint(_) => "RELEASE SAVEPOINT",
        uqa_sql::ast::TransactionStmt::RollbackToSavepoint(_) => "ROLLBACK TO SAVEPOINT",
        uqa_sql::ast::TransactionStmt::CommitAndChain => "COMMIT AND CHAIN",
        uqa_sql::ast::TransactionStmt::RollbackAndChain => "ROLLBACK AND CHAIN",
        _ => unreachable!("only explicit-block transaction commands use this error"),
    };
    SQLError::Routine {
        sqlstate: "25P01".into(),
        message: format!("{command} can only be used in transaction blocks"),
    }
}

pub(super) fn abort_explicit_statement_error(engine: &Engine, error: SQLError) -> SQLError {
    if engine.transaction_depth() == 0 {
        error
    } else {
        engine.abort_sql_transaction_after_error(error)
    }
}

pub(super) fn rollback_implicit_statement(engine: &Engine, action: &str) -> Result<(), SQLError> {
    engine
        .run_transaction_statement(uqa_sql::ast::TransactionStmt::Rollback)
        .map_err(|rollback_error| {
            SQLError::Internal(format!(
                "{action}: autocommit rollback failed: {rollback_error}"
            ))
        })
}

pub(super) fn rollback_after_statement_error<T>(
    engine: &Engine,
    statement_error: SQLError,
) -> Result<T, SQLError> {
    match engine.run_transaction_statement(uqa_sql::ast::TransactionStmt::Rollback) {
        Ok(()) => Err(statement_error),
        Err(rollback_error) => Err(SQLError::Internal(format!(
            "statement failed: {statement_error}; autocommit rollback also failed: {rollback_error}"
        ))),
    }
}
