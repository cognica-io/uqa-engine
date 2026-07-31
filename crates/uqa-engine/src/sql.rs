//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `Engine::sql` driver: parse SQL via `uqa_sql::compile`, lower each
//! statement onto the engine's mutation / search APIs, and roll the
//! result rows into a [`SQLResult`].
//!
//! The SQL surface covers table DDL/DML, indexes, joins, CTEs, windows,
//! aggregates, graph functions, retrieval functions, and engine-registered
//! Rust functions. Unsupported statements return
//! [`uqa_sql::SQLError::Unsupported`] cleanly instead of silently falling
//! through.

#![allow(
    clippy::useless_format,
    clippy::manual_let_else,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::items_after_statements,
    clippy::unnecessary_map_or,
    clippy::match_same_arms,
    clippy::unnested_or_patterns,
    clippy::too_many_lines
)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_core::{DecimalValue, DocId, TemporalValue, Value};
use uqa_sql::ast::{
    AlterTableAction, AlterTableStmt, BinaryOp, ColumnType, CreateIndex, CreateTable, DropKind,
    DropStmt, ForeignKey, ForeignKeyAction, ForeignKeyMatch, SetOpKind, Statement,
};
use uqa_sql::expr::{value_to_tensor, value_to_vector};
use uqa_sql::{compile, ResultRow, SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::Document;

use crate::{Engine, IVFIndexParams, ScoredEntry};

mod age_cypher;
mod aggregates;
mod catalog;
mod correlation;
mod ddl;
mod dml;
mod from_rows;
mod plan_executor;
mod plpgsql_exec;
mod row_functions;
mod scalar;
mod select;
mod volatility;
mod where_eval;
mod window;

pub(crate) use plpgsql_exec::call_user_scalar_function;

use aggregates::{
    aggregate_value, has_aggregate, projection_label_at, AggregateAccumulator,
    PhysicalAggregateExecutor,
};
use catalog::build_info_schema_rows;
use ddl::{
    coerce_to_column_type, column_type_name, core_value_to_json, json_table_arg,
    json_table_value_to_text, json_to_core_value, run_alter_sequence, run_alter_table,
    run_create_index, run_create_sequence, run_create_table, run_create_table_as, run_drop,
    value_to_text,
};
pub(crate) use ddl::{convert_value_to_column_type, validate_vector_dimensions};
use dml::{index_vectors_for_type, run_delete, run_insert, run_merge, run_update};
use from_rows::{
    build_join_spill_with_ctes, engine_func_intercept, prefix_row, ColumnPrune, QualifierFilters,
};
use plan_executor::UnifiedPlanExecutor;
use row_functions::{
    execute_function, execute_function_with_top_k, execute_tree_entries, expect_column_name,
    expect_optional_graph_value, graph_betweenness_entries, graph_hits_entries,
    graph_pagerank_entries, run_age_create_graph_with_evaluator, run_age_drop_graph_with_evaluator,
    run_graph_create_with_evaluator, run_graph_drop_with_evaluator,
    validate_expr_text_match_fields, validate_joined_expr_text_match_fields,
};
pub(crate) use row_functions::{
    run_bayesian_evidence_match_in_execution, run_bayesian_evidence_match_public,
    run_bayesian_match_with_prior_in_execution, run_bayesian_match_with_prior_public,
    run_calibrated_vector_match_public, run_multi_field_match_in_execution,
    run_multi_field_match_public,
};
pub(crate) use select::CteScope;
use select::{
    build_projection_row_with_ctes, expand_star_columns, projection_columns, run_explain,
    ScopedEngineHook,
};
use where_eval::execute_mixed_where;
pub(crate) use where_eval::expr_is_null_free as expr_is_null_free_public;
use window::{has_window, prepare_window_plan, PhysicalWindowExecutor};

type RowUpdateValues = BTreeMap<String, Value>;
type RowUpdateVectors = BTreeMap<String, Vec<Vec<f32>>>;
type RowIndependentUpdateValues = (RowUpdateValues, RowUpdateVectors);

const SCORE_COLUMN: &str = "_score";
const DOC_ID_COLUMN: &str = "_doc_id";
const MERGE_ACTION_COLUMN: &str = "_merge_action";
// NUL cannot occur in a SQL identifier, so this row-carried field cannot
// collide with a user column. Its value is the score emitted by an executed
// retrieval access path; `Null` explicitly marks an ordinary scan.
const SCORE_PROVENANCE_COLUMN: &str = "\0uqa.score_bearing";

fn is_score_provenance_column(column: &str) -> bool {
    column == SCORE_PROVENANCE_COLUMN
        || column
            .strip_suffix(SCORE_PROVENANCE_COLUMN)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

/// Resolve reserved system-schema aliases only when the local name belongs to
/// that schema's built-in surface. Ordinary qualified names stay intact for
/// runtime callbacks and user-defined routine lookup.
pub(crate) fn builtin_function_dispatch_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let Some((schema, local)) = lower.split_once('.') else {
        return lower;
    };
    let is_builtin = match schema {
        "ag_catalog" => matches!(local, "cypher" | "create_graph" | "drop_graph"),
        "pg_catalog" => {
            uqa_sql::registry::is_registered(local)
                || matches!(
                    local,
                    "generate_series"
                        | "unnest"
                        | "regexp_split_to_table"
                        | "string_to_table"
                        | "json_array_elements"
                        | "jsonb_array_elements"
                        | "json_array_elements_text"
                        | "jsonb_array_elements_text"
                        | "json_each"
                        | "jsonb_each"
                        | "json_each_text"
                        | "jsonb_each_text"
                        | "json_object_keys"
                        | "jsonb_object_keys"
                        | "random"
                        | "setseed"
                        | "nextval"
                        | "currval"
                        | "setval"
                        | "current_schema"
                        | "current_schemas"
                )
        }
        _ => false,
    };
    if is_builtin {
        local.to_string()
    } else {
        lower
    }
}

fn doc_id_value(doc_id: DocId) -> Result<Value, SQLError> {
    i64::try_from(doc_id).map(Value::Int).map_err(|_| {
        SQLError::TypeMismatch(format!("document id {doc_id} exceeds the SQL BIGINT range"))
    })
}

fn row_column_value<'a>(row: &'a ResultRow, name: &str) -> Option<&'a Value> {
    if let Some(value) = row.get(name) {
        return Some(value);
    }
    row.iter()
        .find(|(key, _)| key.rsplit_once('.').is_some_and(|(_, col)| col == name))
        .map(|(_, value)| value)
}

pub fn execute(engine: &Engine, sql: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
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

/// SELECT is not synonymous with read-only: UQA exposes a small set of
/// state-changing scalar functions, and SQL/PLpgSQL routines invoked from a
/// projection can contain commands. Classify those plans before choosing the
/// transaction mode so memory execution takes a rollback snapshot and `SQLite`
/// opens a write transaction. Cloning the plan is bounded by query size and
/// avoids the database-sized deep copy paid by a full memory snapshot.
fn query_may_mutate_engine(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
) -> Result<bool, SQLError> {
    query_may_mutate_engine_inner(engine, query, &mut BTreeSet::new())
}

fn query_may_mutate_engine_inner(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
    visiting_views: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    if query_source_may_mutate_engine(engine, query, visiting_views)? {
        return Ok(true);
    }
    let mut plan = uqa_planner::UnifiedPlan::Query(Box::new(query.clone()));
    let mut mutates = false;
    plan.rewrite_scalar_expressions(&mut |expression| {
        let uqa_execution::ScalarExpr::Func { name, .. } = expression else {
            return;
        };
        mutates |= function_may_mutate_engine(engine, name);
    });
    Ok(mutates)
}

fn function_may_mutate_engine(engine: &Engine, name: &str) -> bool {
    let identity = name.to_ascii_lowercase();
    let dispatch_name = builtin_function_dispatch_name(&identity);
    matches!(
        dispatch_name.as_str(),
        "create_analyzer"
            | "drop_analyzer"
            | "set_table_analyzer"
            | "graph_create"
            | "graph_drop"
            | "create_graph"
            | "drop_graph"
            | "cypher"
            | "deep_learn"
            | "bayesian_match"
            | "bayesian_match_with_prior"
            | "fts_match"
            | "multi_field_match"
            | "nextval"
            | "random"
            | "setval"
            | "setseed"
    ) || engine.registered_runtime_function_may_mutate_engine(&identity)
        || engine.lookup_sql_functions(&identity).is_some()
}

fn query_source_may_mutate_engine(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
    visiting_views: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    for cte in &query.ctes {
        if query_source_may_mutate_engine(engine, &cte.query, visiting_views)? {
            return Ok(true);
        }
    }
    match &query.root {
        uqa_planner::RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_ref() {
                if source_may_mutate_engine(engine, source, visiting_views)? {
                    return Ok(true);
                }
            }
            for subquery in &block.subqueries {
                if query_source_may_mutate_engine(engine, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
        }
        uqa_planner::RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            if query_source_may_mutate_engine(engine, left, visiting_views)?
                || query_source_may_mutate_engine(engine, right, visiting_views)?
            {
                return Ok(true);
            }
            for subquery in subqueries {
                if query_source_may_mutate_engine(engine, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
        }
        uqa_planner::RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                if query_source_may_mutate_engine(engine, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn source_may_mutate_engine(
    engine: &Engine,
    source: &uqa_planner::SourcePlan,
    visiting_views: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    match source {
        uqa_planner::SourcePlan::Function { name, .. } => {
            Ok(function_may_mutate_engine(engine, name))
        }
        uqa_planner::SourcePlan::Join { left, right, .. } => {
            Ok(source_may_mutate_engine(engine, left, visiting_views)?
                || source_may_mutate_engine(engine, right, visiting_views)?)
        }
        uqa_planner::SourcePlan::Subquery { body, .. } => {
            query_source_may_mutate_engine(engine, body, visiting_views)
        }
        uqa_planner::SourcePlan::Table { name, .. } => {
            let key = name.to_ascii_lowercase();
            if !visiting_views.insert(key.clone()) {
                return Ok(false);
            }
            let result = match engine.view_plan(name) {
                Ok(Some(view)) => query_may_mutate_engine_inner(engine, &view, visiting_views),
                Ok(None) => Ok(false),
                Err(error) => Err(error),
            };
            visiting_views.remove(&key);
            result
        }
        uqa_planner::SourcePlan::Values { .. } => Ok(false),
    }
}

fn is_transaction_control(plan: &uqa_planner::UnifiedPlan) -> bool {
    matches!(
        plan,
        uqa_planner::UnifiedPlan::Command(command)
            if matches!(command.as_ref(), uqa_planner::CommandPlan::Transaction(_))
    )
}

#[cfg(test)]
fn compile_logical_plans(
    engine: &Engine,
    sql: &str,
) -> Result<Vec<uqa_planner::UnifiedPlan>, SQLError> {
    if let Some(cached) = engine.cached_sql_statement(sql) {
        return Ok(vec![cached.logical_plan.as_ref().clone()]);
    }
    let statements = compile(sql)?;
    let plans = statements
        .iter()
        .cloned()
        .map(|statement| lower_statement(engine, statement))
        .collect::<Vec<_>>();
    if plans.len() == 1 {
        engine.cache_sql_statement(
            sql.to_string(),
            Arc::new(statements[0].clone()),
            Arc::new(plans[0].clone()),
        );
    }
    Ok(plans)
}

fn lower_statement(engine: &Engine, statement: Statement) -> uqa_planner::UnifiedPlan {
    uqa_planner::UnifiedPlan::lower_with(statement, &|name: &str| {
        engine.has_registered_aggregate_function(name)
    })
}

pub(super) fn optimize_engine_plan(
    engine: &Engine,
    plan: uqa_planner::UnifiedPlan,
) -> Result<uqa_planner::UnifiedPlan, SQLError> {
    let callback_error = std::cell::RefCell::new(None);
    let mut optimizer_config = uqa_planner::optimizer::OptimizerConfig::default();
    if volatility::unified_plan_contains_volatile_function(engine, &plan) {
        // Predicate prioritization and DPccp both move expressions across
        // physical evaluation boundaries.  A VOLATILE callback may observe
        // or mutate state on every call, so even a logically equivalent join
        // order can change SQL-visible behavior by changing its call count.
        optimizer_config.enable_filter_pushdown = false;
        optimizer_config.enable_join_reordering = false;
    }
    let optimized = uqa_planner::optimizer::optimize_with_aggregates_and_statistics(
        plan,
        &optimizer_config,
        &|name: &str| engine.has_registered_aggregate_function(name),
        &|table: &str| match engine.try_has_table(table) {
            Ok(false) => None,
            Ok(true) => match (
                engine.table_doc_count(table),
                engine.try_column_stats(table),
            ) {
                (Ok(row_count), Ok(columns)) => {
                    Some(uqa_planner::RelationStats { row_count, columns })
                }
                (Err(error), _) => {
                    *callback_error.borrow_mut() = Some(error);
                    None
                }
                (_, Err(error)) => {
                    *callback_error.borrow_mut() = Some(SQLError::Internal(format!(
                        "read optimizer statistics for `{table}`: {error}"
                    )));
                    None
                }
            },
            Err(error) => {
                *callback_error.borrow_mut() = Some(SQLError::Internal(format!(
                    "resolve optimizer relation `{table}`: {error}"
                )));
                None
            }
        },
    );
    if let Some(error) = callback_error.into_inner() {
        return Err(error);
    }
    optimized.map_err(|error| SQLError::Internal(format!("optimize SQL join order: {error}")))
}

/// Lower and execute an already-compiled statement through the same unified
/// plan entry point used by [`Engine::sql`]. SQL/PLpgSQL routine bodies call
/// this instead of retaining a private AST dispatcher.
pub(super) fn execute_compiled_statement(
    engine: &Engine,
    statement: Statement,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let plan = uqa_planner::UnifiedPlan::lower_with(statement, &|name: &str| {
        engine.has_registered_aggregate_function(name)
    });
    let plan = optimize_engine_plan(engine, plan)?;
    UnifiedPlanExecutor::new(engine, params).execute(&plan)
}

impl Engine {
    /// Run a single SQL statement against the engine.
    pub fn sql(&self, query: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
        let _statement = self.statement_gate.lock();
        self.synchronize_table_catalog()
            .map_err(|err| SQLError::Internal(format!("refresh table catalog: {err}")))?;
        self.synchronize_table_data()
            .map_err(|err| SQLError::Internal(format!("refresh committed table data: {err}")))?;
        self.synchronize_catalog_registries().map_err(|err| {
            SQLError::Internal(format!("refresh durable catalog registries: {err}"))
        })?;
        execute(self, query, params)
    }

    /// All doc ids on a table, used by the SELECT path when there is no
    /// WHERE clause.
    pub fn table_doc_ids(&self, table: &str) -> Result<Vec<uqa_core::DocId>, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let result = t.document_store.read().doc_ids();
        result.map_err(|error| SQLError::Internal(format!("read document ids: {error}")))
    }

    pub(crate) fn table_doc_count(&self, table: &str) -> Result<u64, SQLError> {
        use std::sync::atomic::Ordering;
        let Some(t) = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        if !t.doc_count_dirty.load(Ordering::Acquire) {
            return Ok(t.doc_count_cache.load(Ordering::Acquire));
        }
        let count = t
            .document_store
            .read()
            .len()
            .map_err(|error| SQLError::Internal(format!("read document count: {error}")))?;
        let count = u64::try_from(count)
            .map_err(|_| SQLError::Internal("document count exceeds u64".into()))?;
        t.doc_count_cache.store(count, Ordering::Release);
        t.doc_count_dirty.store(false, Ordering::Release);
        Ok(count)
    }
}

#[cfg(test)]
mod mutability_classifier_tests {
    use super::{
        builtin_function_dispatch_name, compile, lower_statement, query_may_mutate_engine,
        volatility::function_volatility, Engine,
    };
    use crate::{
        SQLAggregateState, SQLFunctionOptions, SQLFunctionVolatility, SQLTableFunctionResult,
    };
    use uqa_core::Value;
    use uqa_planner::UnifiedPlan;
    use uqa_sql::SQLError;

    #[derive(Default)]
    struct NullAggregate;

    impl SQLAggregateState for NullAggregate {
        fn observe(&mut self, _args: &[Value]) -> Result<(), SQLError> {
            Ok(())
        }

        fn finish(&self) -> Result<Value, SQLError> {
            Ok(Value::Null)
        }
    }

    fn query_is_writer(engine: &Engine, sql: &str) -> bool {
        let mut statements = compile(sql).expect("compile callback query");
        assert_eq!(statements.len(), 1);
        let plan = lower_statement(engine, statements.remove(0));
        let UnifiedPlan::Query(query) = plan else {
            panic!("callback SELECT did not lower to a query plan");
        };
        query_may_mutate_engine(engine, &query).expect("classify callback query")
    }

    #[test]
    fn runtime_registered_callbacks_are_writer_classified() {
        let engine = Engine::new();
        engine
            .register_scalar_function("runtime_scalar", |_args: &[Value]| Ok(Value::Null))
            .unwrap();
        engine
            .register_table_function("runtime_table", |_args: &[Value]| {
                Ok(SQLTableFunctionResult::new(
                    ["value"],
                    Vec::<Vec<Value>>::new(),
                ))
            })
            .unwrap();
        engine
            .register_aggregate_function("runtime_aggregate", NullAggregate::default)
            .unwrap();

        assert!(query_is_writer(&engine, "SELECT runtime_scalar() AS value"));
        assert!(query_is_writer(
            &engine,
            "SELECT value FROM runtime_table() AS rows(value)"
        ));
        assert!(query_is_writer(
            &engine,
            "SELECT runtime_aggregate(1) AS value"
        ));

        engine
            .sql(
                "CREATE VIEW runtime_callback_inner AS \
                 SELECT runtime_scalar() AS value",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE VIEW runtime_callback_outer AS \
                 SELECT value FROM runtime_callback_inner",
                &[],
            )
            .unwrap();
        assert!(query_is_writer(
            &engine,
            "SELECT value FROM runtime_callback_outer"
        ));

        engine
            .sql("CREATE SEQUENCE nested_view_sequence START 1", &[])
            .unwrap();
        engine
            .sql(
                "CREATE VIEW sequence_inner AS \
                 SELECT nextval('nested_view_sequence') AS value",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE VIEW sequence_outer AS SELECT value FROM sequence_inner",
                &[],
            )
            .unwrap();
        assert!(query_is_writer(&engine, "SELECT value FROM sequence_outer"));
    }

    #[test]
    fn explicit_runtime_callback_properties_drive_transactions_and_optimization() {
        let engine = Engine::new();
        let immutable_reader = SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable);
        engine
            .register_scalar_function_with_options(
                "runtime_scalar_reader",
                immutable_reader,
                |_args: &[Value]| Ok(Value::Null),
            )
            .unwrap();
        engine
            .register_table_function_with_options(
                "runtime_table_reader",
                immutable_reader,
                |_args: &[Value]| {
                    Ok(SQLTableFunctionResult::new(
                        ["value"],
                        Vec::<Vec<Value>>::new(),
                    ))
                },
            )
            .unwrap();
        engine
            .register_aggregate_function_with_options(
                "runtime_aggregate_reader",
                immutable_reader,
                NullAggregate::default,
            )
            .unwrap();

        assert!(!query_is_writer(
            &engine,
            "SELECT runtime_scalar_reader() AS value"
        ));
        assert!(!query_is_writer(
            &engine,
            "SELECT value FROM runtime_table_reader() AS rows(value)"
        ));
        assert!(!query_is_writer(
            &engine,
            "SELECT runtime_aggregate_reader(1) AS value"
        ));
        assert_eq!(
            function_volatility(&engine, "runtime_scalar_reader", 0),
            SQLFunctionVolatility::Immutable
        );

        let invalid = SQLFunctionOptions::new(SQLFunctionVolatility::Stable, true);
        let error = engine
            .register_scalar_function_with_options(
                "invalid_mutating_stable",
                invalid,
                |_args: &[Value]| Ok(Value::Null),
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("may mutate engine state must be VOLATILE"));
    }

    #[test]
    fn reserved_catalog_aliases_resolve_only_existing_builtins() {
        assert_eq!(
            builtin_function_dispatch_name("ag_catalog.cypher"),
            "cypher"
        );
        assert_eq!(
            builtin_function_dispatch_name("pg_catalog.generate_series"),
            "generate_series"
        );
        assert_eq!(
            builtin_function_dispatch_name("ag_catalog.generate_series"),
            "ag_catalog.generate_series"
        );
        assert_eq!(
            builtin_function_dispatch_name("application.cypher"),
            "application.cypher"
        );

        let engine = Engine::new();
        assert!(query_is_writer(
            &engine,
            "SELECT * FROM ag_catalog.cypher('g', $$CREATE (n)$$) AS (v agtype)"
        ));
        assert_eq!(
            function_volatility(&engine, "ag_catalog.cypher", 2),
            SQLFunctionVolatility::Volatile
        );
    }
}

#[cfg(test)]
mod unified_plan_tests {
    use uqa_planner::{CommandPlan, ComputePlan, RelationalPlan, SourcePlan, UnifiedPlan};

    use super::{compile_logical_plans, doc_id_value, optimize_engine_plan, Engine};

    #[test]
    fn document_ids_outside_bigint_are_rejected_at_the_sql_boundary() {
        assert!(doc_id_value(i64::MAX as u64).is_ok());
        assert!(doc_id_value(i64::MAX as u64 + 1).is_err());
    }

    fn one(engine: &Engine, sql: &str) -> UnifiedPlan {
        let mut plans = compile_logical_plans(engine, sql).expect("statement plans");
        assert_eq!(plans.len(), 1);
        optimize_engine_plan(engine, plans.remove(0)).expect("optimized statement plan")
    }

    #[test]
    fn sql_boundaries_are_cached_as_structural_unified_plans() {
        let engine = Engine::new();

        let arithmetic = one(&engine, "SELECT amount * 2 + 1 AS adjusted FROM ledger");
        let UnifiedPlan::Query(query) = arithmetic else {
            panic!("arithmetic SELECT must be a QueryPlan");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block");
        };
        assert!(matches!(block.compute, ComputePlan::Project));

        let window = one(
            &engine,
            "SELECT row_number() OVER (PARTITION BY account ORDER BY amount) FROM ledger",
        );
        let UnifiedPlan::Query(query) = window else {
            panic!("window SELECT must be a QueryPlan");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block");
        };
        assert!(matches!(block.compute, ComputePlan::Window));

        let subquery = one(
            &engine,
            "SELECT q.total FROM (SELECT sum(amount) AS total FROM ledger) AS q",
        );
        let UnifiedPlan::Query(query) = subquery else {
            panic!("subquery SELECT must be a QueryPlan");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block");
        };
        assert!(matches!(block.from, Some(SourcePlan::Subquery { .. })));

        let mutation = one(
            &engine,
            "UPDATE ledger SET amount = amount + 1 WHERE amount > 0",
        );
        assert!(matches!(
            mutation,
            UnifiedPlan::Command(command) if matches!(*command, CommandPlan::Update(_))
        ));

        // The cache retains the structural IR alongside the parsed statement;
        // execution still enters exclusively through UnifiedPlanExecutor.
        assert!(engine
            .cached_sql_plans("SELECT amount * 2 + 1 AS adjusted FROM ledger")
            .is_some_and(|plans| matches!(plans.as_slice(), [UnifiedPlan::Query(_)])));
    }

    #[test]
    fn memory_read_only_statements_reuse_optimized_plan_until_invalidation() {
        let engine = Engine::new();
        engine
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create table");
        engine
            .sql("INSERT INTO items (id) VALUES (1), (2)", &[])
            .expect("seed rows");
        let query = "SELECT id FROM items WHERE id > 0 ORDER BY id";

        engine.sql(query, &[]).expect("warm statement cache");
        let first = engine
            .cached_sql_statement(query)
            .and_then(|cached| cached.optimized_plan)
            .expect("memory read caches its optimized plan");

        engine.sql(query, &[]).expect("reuse statement cache");
        let second = engine
            .cached_sql_statement(query)
            .and_then(|cached| cached.optimized_plan)
            .expect("optimized plan remains cached");
        assert!(std::sync::Arc::ptr_eq(&first, &second));

        engine
            .sql("INSERT INTO items (id) VALUES (3)", &[])
            .expect("mutate table");
        assert!(
            engine.cached_sql_statement(query).is_none(),
            "a committed data change must invalidate the optimized plan"
        );
    }

    #[test]
    fn snapshot_scoped_statements_do_not_cache_optimized_plans() {
        let dir = tempfile::tempdir().expect("temporary database directory");
        let persistent =
            Engine::open(&dir.path().join("statement-optimized-cache.db")).expect("open engine");
        persistent
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create table");
        persistent
            .sql("INSERT INTO items (id) VALUES (1), (2)", &[])
            .expect("seed rows");
        let persistent_query = "SELECT id FROM items WHERE id > 0 ORDER BY id";
        persistent
            .sql(persistent_query, &[])
            .expect("run persistent read");
        assert!(
            persistent
                .cached_sql_statement(persistent_query)
                .is_some_and(|cached| cached.optimized_plan.is_none()),
            "persistent reads must optimize inside each storage snapshot"
        );

        let memory = Engine::new();
        memory
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create memory table");
        memory.begin().expect("begin explicit transaction");
        let transactional_query = "SELECT id FROM items ORDER BY id";
        memory
            .sql(transactional_query, &[])
            .expect("run explicit-transaction read");
        assert!(
            memory
                .cached_sql_statement(transactional_query)
                .is_some_and(|cached| cached.optimized_plan.is_none()),
            "explicit transactions must optimize against their current state"
        );
        memory.rollback().expect("rollback explicit transaction");
    }

    #[test]
    fn views_retain_their_compiled_query_plan() {
        let engine = Engine::new();
        engine
            .sql("CREATE TABLE ledger (amount INTEGER)", &[])
            .expect("view source table");
        engine
            .sql(
                "CREATE VIEW ledger_totals AS SELECT sum(amount) AS total FROM ledger",
                &[],
            )
            .expect("view definition");

        let plan = engine
            .view_plan("ledger_totals")
            .expect("view catalog read")
            .expect("compiled view plan");
        let RelationalPlan::QueryBlock(block) = plan.root else {
            panic!("view must retain a query block");
        };
        assert!(matches!(block.compute, ComputePlan::Aggregate));
    }

    #[test]
    fn committed_table_data_invalidates_sibling_statement_plans_but_rollback_does_not() {
        let dir = tempfile::tempdir().expect("temporary database directory");
        let root = Engine::open(&dir.path().join("statement-data-epoch.db"))
            .expect("open persistent engine");
        root.sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create table");
        let writer = root.new_session().expect("writer session");
        let observer = root.new_session().expect("observer session");
        let query = "SELECT id FROM items WHERE id = 1";

        let version_before_query = observer
            .sqlite_session
            .as_ref()
            .expect("persistent observer")
            .data_version()
            .expect("read SQLite data version");
        observer.sql(query, &[]).expect("warm statement plan");
        assert_eq!(
            observer
                .sqlite_session
                .as_ref()
                .expect("persistent observer")
                .data_version()
                .expect("read SQLite data version"),
            version_before_query,
            "a read-only statement persisted an alias-scoped value index"
        );
        assert!(observer
            .backend
            .as_ref()
            .expect("persistent observer")
            .btree_index_fields("items")
            .expect("read unqualified value-index fields")
            .is_empty());
        assert_eq!(
            observer
                .backend
                .as_ref()
                .expect("persistent observer")
                .btree_index_fields("public.items")
                .expect("read canonical value-index fields"),
            vec!["id"]
        );
        assert!(observer.cached_sql_plans(query).is_some());
        assert_eq!(
            observer
                .seen_table_data_epoch
                .load(std::sync::atomic::Ordering::Acquire),
            observer
                .table_data_epoch
                .load(std::sync::atomic::Ordering::Acquire),
            "the completed read statement left its in-process data generation stale"
        );
        assert_eq!(
            Some(
                observer
                    .seen_sqlite_data_version
                    .load(std::sync::atomic::Ordering::Acquire)
            ),
            observer
                .sqlite_session
                .as_ref()
                .expect("persistent observer")
                .data_version()
                .expect("read SQLite data version"),
            "the completed read statement left its SQLite data version stale"
        );
        assert_eq!(observer.table_doc_count("items").expect("warm count"), 0);
        assert!(
            observer.cached_sql_plans(query).is_some(),
            "reading the observer's table count cleared its statement cache"
        );
        let epoch_before_rollback = observer
            .table_data_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let data_version_before_rollback = observer
            .sqlite_session
            .as_ref()
            .expect("persistent observer")
            .data_version()
            .expect("read SQLite data version");
        assert_eq!(
            observer
                .seen_table_data_epoch
                .load(std::sync::atomic::Ordering::Acquire),
            epoch_before_rollback,
            "the warmed observer did not consume the current in-process data generation"
        );
        assert_eq!(
            Some(
                observer
                    .seen_sqlite_data_version
                    .load(std::sync::atomic::Ordering::Acquire)
            ),
            data_version_before_rollback,
            "the warmed observer did not consume the current SQLite data version"
        );

        writer.begin().expect("begin rolled-back write");
        assert!(
            observer.cached_sql_plans(query).is_some(),
            "starting a sibling transaction cleared the observer statement cache"
        );
        writer
            .sql("INSERT INTO items (id) VALUES (1)", &[])
            .expect("insert rolled-back row");
        assert!(
            observer.cached_sql_plans(query).is_some(),
            "an uncommitted sibling write cleared the observer statement cache"
        );
        writer.rollback().expect("rollback write");
        assert_eq!(
            observer
                .table_data_epoch
                .load(std::sync::atomic::Ordering::Acquire),
            epoch_before_rollback,
            "a rolled-back mutation published an in-process data generation"
        );
        assert_eq!(
            observer
                .sqlite_session
                .as_ref()
                .expect("persistent observer")
                .data_version()
                .expect("read SQLite data version"),
            data_version_before_rollback,
            "a rolled-back mutation changed SQLite's committed data version"
        );
        assert_eq!(
            observer
                .seen_table_data_epoch
                .load(std::sync::atomic::Ordering::Acquire),
            epoch_before_rollback,
            "a rolled-back mutation changed the observer's consumed data generation"
        );
        assert_eq!(
            Some(
                observer
                    .seen_sqlite_data_version
                    .load(std::sync::atomic::Ordering::Acquire)
            ),
            data_version_before_rollback,
            "a rolled-back mutation changed the observer's consumed SQLite data version"
        );
        assert!(
            observer.cached_sql_plans(query).is_some(),
            "the writer cleared a sibling statement cache before synchronization"
        );
        observer
            .synchronize_table_data()
            .expect("check unchanged generation");
        assert!(
            observer.cached_sql_plans(query).is_some(),
            "a rolled-back mutation must not publish a cache generation"
        );
        assert_eq!(observer.table_doc_count("items").expect("cached count"), 0);

        writer.begin().expect("begin committed write");
        writer
            .sql("INSERT INTO items (id) VALUES (1)", &[])
            .expect("insert committed row");
        writer.commit().expect("commit write");
        assert!(observer.cached_sql_plans(query).is_some());
        observer
            .synchronize_table_data()
            .expect("refresh committed generation");
        assert!(
            observer.cached_sql_plans(query).is_none(),
            "a sibling commit must invalidate optimized statement plans"
        );
        assert_eq!(
            observer.table_doc_count("items").expect("refreshed count"),
            1
        );
    }

    #[test]
    fn sibling_catalog_commit_invalidates_cached_logical_statements() {
        let dir = tempfile::tempdir().expect("temporary database directory");
        let root = Engine::open(&dir.path().join("statement-catalog-epoch.db"))
            .expect("open persistent engine");
        root.sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create table");
        let writer = root.new_session().expect("writer session");
        let observer = root.new_session().expect("observer session");
        let query = "SELECT id FROM items WHERE id = 1";

        observer.sql(query, &[]).expect("warm logical statement");
        assert!(observer.cached_sql_plans(query).is_some());
        writer
            .sql("ALTER TABLE items ADD COLUMN label TEXT", &[])
            .expect("commit sibling DDL");
        observer
            .synchronize_table_catalog()
            .expect("refresh sibling table catalog");
        assert!(observer.cached_sql_plans(query).is_none());
        assert!(observer.table_has_column("items", "label").unwrap());
    }

    #[test]
    fn multi_statement_batch_lowers_each_statement_after_prior_catalog_changes() {
        let engine = Engine::new();
        let result = engine
            .sql(
                "CREATE SCHEMA batch_ns; \
                 CREATE TABLE batch_ns.items (id INTEGER PRIMARY KEY); \
                 SET search_path TO batch_ns; \
                 INSERT INTO items (id) VALUES (7); \
                 SELECT id FROM items",
                &[],
            )
            .expect("execute dependent statements in one parsed batch");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0]["id"], uqa_core::Value::Int(7));
    }

    #[test]
    fn multi_statement_batch_observes_function_create_and_drop() {
        let engine = Engine::new();
        let created = engine
            .sql(
                "CREATE FUNCTION batch_inc(a INTEGER) RETURNS INTEGER RETURN a + 1; \
                 SELECT batch_inc(4) AS value",
                &[],
            )
            .expect("call function created by the preceding statement");
        assert_eq!(created.rows[0]["value"], uqa_core::Value::Int(5));

        let error = engine
            .sql(
                "DROP FUNCTION batch_inc(INTEGER); SELECT batch_inc(4) AS value",
                &[],
            )
            .expect_err("dropped function must not survive as a stale lowered plan");
        assert!(error.to_string().contains("batch_inc"));
    }
}
