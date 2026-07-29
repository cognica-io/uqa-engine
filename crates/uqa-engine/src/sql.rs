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

use uqa_core::{DecimalValue, DocId, TemporalValue, Value};
use uqa_execution::ScalarExpr;
use uqa_sql::ast::{
    AlterTableAction, AlterTableStmt, BinaryOp, ColumnDef as SQLColumnDef, ColumnType, CreateIndex,
    CreateTable, DropKind, DropStmt, ForeignKey, ForeignKeyAction, ForeignKeyMatch, SetOpKind,
    Statement,
};
use uqa_sql::expr::{value_to_tensor, value_to_vector};
use uqa_sql::{compile, ResultRow, SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::Document;

use crate::{Engine, IVFIndexParams, ScoredEntry};

mod age_cypher;
mod aggregates;
mod catalog;
mod ddl;
mod dml;
mod from_rows;
mod plan_executor;
mod plpgsql_exec;
mod row_functions;
mod scalar;
mod select;
mod where_eval;
mod window;

pub(crate) use plpgsql_exec::call_user_scalar_function;

use aggregates::{
    aggregate_join_rows, aggregate_value, build_aggregate_rows, has_aggregate, projection_label_at,
    AggregateAccumulator,
};
use catalog::build_info_schema_rows;
use ddl::{
    coerce_to_column_type, column_type_name, core_value_to_json, json_table_arg,
    json_table_value_to_text, json_to_core_value, run_alter_sequence, run_alter_table,
    run_create_index, run_create_sequence, run_create_table, run_create_table_as, run_drop,
    validate_vector_dimensions, value_to_text,
};
use dml::{index_vectors_for_type, run_delete, run_insert, run_merge, run_update};
use from_rows::{
    build_join_rows_with_ctes, build_join_rows_with_ctes_filtered,
    build_join_rows_with_ctes_filtered_by_qualifier,
    build_join_rows_with_ctes_filtered_filtered_by_qualifier,
    build_join_rows_with_ctes_filtered_pruned,
    build_join_rows_with_ctes_filtered_pruned_filtered_by_qualifier,
    build_join_rows_with_ctes_pruned, build_join_rows_with_ctes_pruned_filtered_by_qualifier,
    engine_func_intercept, execute_lateral_subquery, prefix_row, project_join_row_with_plan,
    ColumnPrune, QualifierFilters,
};
use plan_executor::UnifiedPlanExecutor;
use row_functions::{
    execute_function, execute_function_with_top_k, expect_column_name, expect_optional_graph_value,
    graph_betweenness_entries, graph_hits_entries, graph_pagerank_entries,
    run_age_create_graph_with_evaluator, run_age_drop_graph_with_evaluator,
    run_graph_create_with_evaluator, run_graph_drop_with_evaluator,
    validate_expr_text_match_fields, validate_joined_expr_text_match_fields,
};
pub(crate) use row_functions::{
    run_bayesian_evidence_match_public, run_bayesian_match_public,
    run_bayesian_match_with_prior_public, run_calibrated_vector_match_public, run_knn_match_public,
    run_multi_field_match_public, run_text_match_public,
};
pub(crate) use select::CteScope;
use select::{
    apply_row_order_limit_with_ctes, build_projection_row_with_ctes, expand_star_columns,
    projection_columns, run_explain, ScopedEngineHook,
};
use where_eval::execute_mixed_where;
pub(crate) use where_eval::expr_is_null_free as expr_is_null_free_public;
use window::{compute_window_columns, has_window};

type RowUpdateValues = BTreeMap<String, Value>;
type RowUpdateVectors = BTreeMap<String, Vec<Vec<f32>>>;
type RowIndependentUpdateValues = (RowUpdateValues, RowUpdateVectors);

const SCORE_COLUMN: &str = "_score";
const DOC_ID_COLUMN: &str = "_doc_id";
const MERGE_ACTION_COLUMN: &str = "_merge_action";

fn projected_value_from_row(expr: &ScalarExpr, row: &ResultRow) -> Option<Value> {
    match expr {
        ScalarExpr::Column(name) => {
            Some(row_column_value(row, name).cloned().unwrap_or(Value::Null))
        }
        ScalarExpr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => {
            let value = if key.is_empty() {
                let lookup = format!("{qualifier}.{column}");
                row.get(&lookup)
            } else {
                row.get(key)
            };
            let value = value.or_else(|| uqa_sql::expr::unqualified_fallback(row, column));
            Some(value.cloned().unwrap_or(Value::Null))
        }
        _ => None,
    }
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
    let plans = compile_optimized_plans(engine, sql)?;
    if plans.is_empty() {
        return Ok(SQLResult::empty());
    }
    let mut executor = UnifiedPlanExecutor::new(engine, params);
    let mut last = SQLResult::empty();
    for plan in &plans {
        engine.cancellation_token().check()?;
        last = executor.execute(plan)?;
    }
    Ok(last)
}

fn compile_optimized_plans(
    engine: &Engine,
    sql: &str,
) -> Result<Vec<uqa_planner::UnifiedPlan>, SQLError> {
    if let Some(plans) = engine.cached_sql_plans(sql) {
        return Ok(plans);
    }
    let plans = compile(sql)?
        .into_iter()
        .map(|statement| {
            let plan = uqa_planner::UnifiedPlan::lower_with(statement, &|name: &str| {
                engine.has_registered_aggregate_function(name)
            });
            uqa_planner::optimizer::optimize_with_aggregates(
                plan,
                &uqa_planner::optimizer::OptimizerConfig::default(),
                &|name: &str| engine.has_registered_aggregate_function(name),
            )
        })
        .collect::<Vec<_>>();
    engine.cache_sql_plans(sql.to_string(), plans.clone());
    Ok(plans)
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
    let plan = uqa_planner::optimizer::optimize_with_aggregates(
        plan,
        &uqa_planner::optimizer::OptimizerConfig::default(),
        &|name: &str| engine.has_registered_aggregate_function(name),
    );
    UnifiedPlanExecutor::new(engine, params).execute(&plan)
}

impl Engine {
    /// Run a single SQL statement against the engine.
    pub fn sql(&self, query: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
        execute(self, query, params)
    }

    /// All doc ids on a table, used by the SELECT path when there is no
    /// WHERE clause.
    pub fn table_doc_ids(&self, table: &str) -> Vec<uqa_core::DocId> {
        let Some(t) = self.table(table) else {
            return Vec::new();
        };
        let ids = t.document_store.read().doc_ids();
        ids
    }

    pub(crate) fn table_doc_count(&self, table: &str) -> u64 {
        use std::sync::atomic::Ordering;
        let Some(t) = self.table(table) else {
            return 0;
        };
        if !t.doc_count_dirty.load(Ordering::Acquire) {
            return t.doc_count_cache.load(Ordering::Acquire);
        }
        let count = t.document_store.read().len() as u64;
        t.doc_count_cache.store(count, Ordering::Release);
        t.doc_count_dirty.store(false, Ordering::Release);
        count
    }
}

#[cfg(test)]
mod unified_plan_tests {
    use uqa_planner::{CommandPlan, ComputePlan, RelationalPlan, SourcePlan, UnifiedPlan};

    use super::{compile_optimized_plans, Engine};

    fn one(engine: &Engine, sql: &str) -> UnifiedPlan {
        let mut plans = compile_optimized_plans(engine, sql).expect("statement plans");
        assert_eq!(plans.len(), 1);
        plans.remove(0)
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

        // The cache contains the same IR, not a parsed Statement that could
        // re-enter a separate dispatcher.
        assert!(engine
            .cached_sql_plans("SELECT amount * 2 + 1 AS adjusted FROM ledger")
            .is_some_and(|plans| matches!(plans.as_slice(), [UnifiedPlan::Query(_)])));
    }

    #[test]
    fn views_retain_their_compiled_query_plan() {
        let engine = Engine::new();
        engine
            .sql(
                "CREATE VIEW ledger_totals AS SELECT sum(amount) AS total FROM ledger",
                &[],
            )
            .expect("view definition");

        let plan = engine
            .view_plan("ledger_totals")
            .expect("compiled view plan");
        let RelationalPlan::QueryBlock(block) = plan.root else {
            panic!("view must retain a query block");
        };
        assert!(matches!(block.compute, ComputePlan::Aggregate));
    }
}
