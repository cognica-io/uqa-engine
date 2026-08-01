use super::{
    compile, is_transaction_control, lower_statement, optimize_engine_plan,
    query_may_mutate_engine, Arc, Engine, SQLError, SQLParam, SQLResult, UnifiedPlanExecutor,
};

pub(crate) fn execute(
    engine: &Engine,
    sql: &str,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    // Reject cancelled tokens up-front so a stale cancel signal does
    // not leak into a fresh batch. Callers that want the
    // cancellation flag preserved across statements should use
    // [`crate::Engine::reset_cancellation`] explicitly between calls.
    engine.cancellation_token().check()?;
    // Parse an uncached batch completely before executing its first statement.
    // This preserves syntax atomicity. Exact single-statement cache hits reuse
    // the parsed AST and logical plan; batches still lower each statement only
    // when its turn arrives so earlier DDL, SET, ANALYZE, and function commands
    // can affect the following statement's semantics.
    let cached_statement = engine.cached_sql_statement(sql);
    let (statements, mut cached_entry) = match cached_statement {
        Some(cached) => (vec![cached.statement.as_ref().clone()], Some(cached)),
        None => (compile(sql)?, None),
    };
    if statements.is_empty() {
        return Ok(SQLResult::empty());
    }
    let is_single_statement = statements.len() == 1;
    let mut executor = UnifiedPlanExecutor::new(engine, params);
    let mut last = SQLResult::empty();
    for statement in statements {
        engine.cancellation_token().check()?;
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
            last = executor.execute(initial_plan.as_ref())?;
            continue;
        }

        if engine.transaction_depth() != 0 {
            // The statement was lowered immediately above, after any earlier
            // BEGIN or catalog-changing statement in this batch completed.
            let optimized = optimize_engine_plan(engine, initial_plan.as_ref().clone())?;
            last = executor.execute(&optimized)?;
            continue;
        }

        // Every persistent SQL statement owns one storage transaction when
        // the caller has not opened an explicit transaction. This is the
        // actual autocommit boundary: catalog, document, FTS, btree, vector,
        // graph, and registry writes either commit together or are all rolled
        // back. Memory commands use the same boundary so a fallible multi-row
        // mutation restores its pre-statement snapshot; read-only memory
        // queries avoid copying the whole database.
        let is_read_query = match initial_plan.as_ref() {
            uqa_planner::UnifiedPlan::Query(query) => !query_may_mutate_engine(engine, query)?,
            uqa_planner::UnifiedPlan::Command(_) => false,
        };
        let needs_implicit_transaction = engine.backend.is_some() || !is_read_query;
        if needs_implicit_transaction {
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
            let must_restart_as_writer = if is_read_query && engine.backend.is_some() {
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
            let optimized = match optimize_engine_plan(engine, plan) {
                Ok(plan) => plan,
                Err(error) => return rollback_after_statement_error(engine, error),
            };
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
                let plan = Arc::new(optimize_engine_plan(engine, initial_plan.as_ref().clone())?);
                if is_single_statement {
                    engine.cache_optimized_sql_plan(sql, Arc::clone(&plan));
                }
                plan
            };
            last = executor.execute(optimized.as_ref())?;
        }
    }
    Ok(last)
}

fn rollback_implicit_statement(engine: &Engine, action: &str) -> Result<(), SQLError> {
    engine
        .run_transaction_statement(uqa_sql::ast::TransactionStmt::Rollback)
        .map_err(|rollback_error| {
            SQLError::Internal(format!(
                "{action}: autocommit rollback failed: {rollback_error}"
            ))
        })
}

fn rollback_after_statement_error(
    engine: &Engine,
    statement_error: SQLError,
) -> Result<SQLResult, SQLError> {
    match engine.run_transaction_statement(uqa_sql::ast::TransactionStmt::Rollback) {
        Ok(()) => Err(statement_error),
        Err(rollback_error) => Err(SQLError::Internal(format!(
            "statement failed: {statement_error}; autocommit rollback also failed: {rollback_error}"
        ))),
    }
}
