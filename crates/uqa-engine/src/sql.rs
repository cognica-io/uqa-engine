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
use uqa_sql::ast::{
    AlterTableAction, AlterTableStmt, BinaryOp, ColumnDef as SQLColumnDef, ColumnType, CreateIndex,
    CreateTable, DeleteStmt, DropKind, DropStmt, Expr, ForeignKey, ForeignKeyAction,
    ForeignKeyMatch, FromClause, InsertStmt, OrderBy, Projection, SelectStmt, SetOpKind, Statement,
    UpdateStmt, WindowSpec, CTE,
};
use uqa_sql::expr::{eval, value_to_tensor, value_to_vector, EvalContext};
use uqa_sql::{compile, ResultRow, SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::Document;

use crate::{Engine, IVFIndexParams, ScoredEntry};

mod age_cypher;
mod aggregates;
mod catalog;
mod ddl;
mod dml;
mod from_rows;
mod row_functions;
mod select;
mod where_eval;
mod window;

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
    engine_func_intercept, execute_lateral_subquery, prefix_row, project_join_row_with_engine,
    project_join_row_with_hook, project_join_row_with_hook_and_labels, ColumnPrune,
    QualifierFilters,
};
use row_functions::{
    execute_function, execute_function_with_top_k, expect_column_name, expect_optional_graph_value,
    graph_betweenness_entries, graph_hits_entries, graph_pagerank_entries, run_graph_create,
    run_graph_drop, validate_expr_text_match_fields, validate_joined_expr_text_match_fields,
};
pub(crate) use row_functions::{
    run_bayesian_match_public, run_bayesian_match_with_prior_public,
    run_calibrated_vector_match_public, run_knn_match_public, run_multi_field_match_public,
    run_text_match_public,
};
use select::{
    apply_row_order_limit, build_projection_row, execute_select, expand_star_columns,
    materialize_cte_list, materialize_ctes, projection_columns, run_explain, run_select,
    ScopedEngineHook,
};
pub(crate) use select::{run_correlated_subquery, CteScope};
use where_eval::execute_mixed_where;
use window::{compute_window_columns, has_window};

type RowUpdateValues = BTreeMap<String, Value>;
type RowUpdateVectors = BTreeMap<String, Vec<Vec<f32>>>;
type RowIndependentUpdateValues = (RowUpdateValues, RowUpdateVectors);

const SCORE_COLUMN: &str = "_score";
const DOC_ID_COLUMN: &str = "_doc_id";
const MERGE_ACTION_COLUMN: &str = "_merge_action";

fn projected_value_from_row(expr: &Expr, row: &ResultRow) -> Option<Value> {
    match expr {
        Expr::Column(name) => Some(row_column_value(row, name).cloned().unwrap_or(Value::Null)),
        Expr::QualifiedColumn {
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
    let stmts = compile_optimized_statements(engine, sql)?;
    if stmts.is_empty() {
        return Ok(SQLResult::empty());
    }
    let mut last = SQLResult::empty();
    for stmt in stmts {
        engine.cancellation_token().check()?;
        last = run_optimized_stmt(engine, stmt, params)?;
    }
    Ok(last)
}

fn compile_optimized_statements(engine: &Engine, sql: &str) -> Result<Vec<Statement>, SQLError> {
    if let Some(statements) = engine.cached_sql_statements(sql) {
        return Ok(statements);
    }
    let statements = compile(sql)?
        .into_iter()
        .map(optimize_statement)
        .collect::<Vec<_>>();
    engine.cache_sql_statements(sql.to_string(), statements.clone());
    Ok(statements)
}

fn run_optimized_stmt(
    engine: &Engine,
    stmt: Statement,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    match stmt {
        Statement::CreateTable(c) => run_create_table(engine, c),
        Statement::CreateIndex(c) => run_create_index(engine, c),
        Statement::Insert(i) => run_insert(engine, i, params),
        Statement::Select(s) => run_select(engine, *s, params),
        Statement::Update(u) => run_update(engine, u, params),
        Statement::Delete(d) => run_delete(engine, d, params),
        Statement::Drop(d) => run_drop(engine, d),
        Statement::AlterTable(a) => run_alter_table(engine, a),
        Statement::CreateView {
            name,
            body,
            or_replace,
        } => {
            if engine.has_table(&name) {
                return Err(SQLError::Unsupported(format!(
                    "CREATE VIEW: relation `{name}` already exists as a table"
                )));
            }
            if !or_replace && engine.view(&name).is_some() {
                return Err(SQLError::Unsupported(format!(
                    "CREATE VIEW: relation `{name}` already exists"
                )));
            }
            engine.register_view(&name, *body);
            Ok(SQLResult::empty())
        }
        Statement::CreateSchema {
            name,
            if_not_exists,
        } => {
            engine.register_schema(&name, if_not_exists);
            Ok(SQLResult::empty())
        }
        Statement::Explain { body, .. } => run_explain(engine, *body, params),
        Statement::SetVariable { name, value } => {
            engine.set_variable(&name, &value);
            Ok(SQLResult::empty())
        }
        Statement::ShowVariable { name } => {
            let value = engine.show_variable(&name);
            let mut row = ResultRow::new();
            row.insert(name.clone(), Value::Str(value));
            Ok(SQLResult {
                columns: vec![name],
                rows: vec![row],
                affected_rows: 0,
            })
        }
        Statement::Discard { target } => {
            engine.discard(target);
            Ok(SQLResult::empty())
        }
        Statement::Analyze { table } => {
            engine.run_analyze(table.as_deref());
            Ok(SQLResult::empty())
        }
        Statement::Truncate { tables, .. } => {
            for t in &tables {
                if !engine.has_table(t) {
                    return Err(SQLError::Unsupported(format!(
                        "TRUNCATE TABLE: relation `{t}` does not exist"
                    )));
                }
                engine.truncate_table(t);
            }
            Ok(SQLResult::empty())
        }
        Statement::Transaction(tx) => {
            engine.run_transaction_statement(tx)?;
            Ok(SQLResult::empty())
        }
        Statement::CreateSequence(s) => run_create_sequence(engine, s),
        Statement::AlterSequence(s) => run_alter_sequence(engine, s),
        Statement::CreateTableAs {
            name,
            if_not_exists,
            body,
        } => run_create_table_as(engine, name, if_not_exists, *body, params),
        Statement::Prepare { name, body } => {
            if engine.lookup_prepared(&name).is_some() {
                return Err(SQLError::Unsupported(format!(
                    "Prepared statement `{name}` already exists"
                )));
            }
            engine.register_prepared(name, optimize_statement(*body));
            Ok(SQLResult::empty())
        }
        Statement::Execute { name, params: ps } => run_execute_prepared(engine, &name, &ps, params),
        Statement::Deallocate { name } => {
            if let Some(ref n) = name {
                if engine.lookup_prepared(n).is_none() {
                    return Err(SQLError::Unsupported(format!(
                        "Prepared statement `{n}` does not exist"
                    )));
                }
            }
            engine.deallocate_prepared(name.as_deref());
            Ok(SQLResult::empty())
        }
        Statement::Values { rows } => run_values(engine, rows, params),
        Statement::CreateForeignServer(s) => {
            engine
                .register_foreign_server(s.name, s.fdw_type, s.options, s.if_not_exists)
                .map_err(SQLError::Unsupported)?;
            Ok(SQLResult::empty())
        }
        Statement::CreateForeignTable(s) => {
            engine
                .register_foreign_table(
                    s.name,
                    s.server_name,
                    s.columns,
                    s.options,
                    s.if_not_exists,
                )
                .map_err(SQLError::Unsupported)?;
            Ok(SQLResult::empty())
        }
        Statement::Merge(m) => run_merge(engine, m, params),
    }
}

fn run_execute_prepared(
    engine: &Engine,
    name: &str,
    args: &[uqa_sql::ast::Expr],
    outer_params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let stmt = engine.lookup_prepared(name).ok_or_else(|| {
        SQLError::Unsupported(format!("Prepared statement `{name}` does not exist"))
    })?;
    let ctx = uqa_sql::expr::EvalContext::new(None, outer_params).with_engine(engine);
    let mut bound: Vec<SQLParam> = Vec::with_capacity(args.len());
    for a in args {
        let v = uqa_sql::expr::eval(a, &ctx)?;
        bound.push(SQLParam::Scalar(v));
    }
    run_optimized_stmt(engine, stmt, &bound)
}

fn run_values(
    engine: &Engine,
    rows: Vec<Vec<uqa_sql::ast::Expr>>,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    use uqa_sql::expr::{eval, EvalContext};
    let ctx = EvalContext::new(None, params).with_engine(engine);
    if rows.is_empty() {
        return Ok(SQLResult::empty());
    }
    let columns: Vec<String> = (0..rows[0].len())
        .map(|i| format!("column{}", i + 1))
        .collect();
    let mut out_rows: Vec<uqa_sql::result::ResultRow> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut r = uqa_sql::result::ResultRow::new();
        for (i, expr) in row.iter().enumerate() {
            let v = eval(expr, &ctx)?;
            r.insert(columns[i].clone(), v);
        }
        out_rows.push(r);
    }
    Ok(SQLResult {
        columns,
        rows: out_rows,
        affected_rows: 0,
    })
}

fn optimize_statement(stmt: Statement) -> Statement {
    use uqa_planner::optimizer::{optimize, OptimizerConfig};
    let cfg = OptimizerConfig::default();
    match stmt {
        Statement::Select(s) => Statement::Select(Box::new(optimize(*s, &cfg))),
        other => other,
    }
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
        let Some(t) = self.table(table) else {
            return 0;
        };
        let count = t.document_store.read().len() as u64;
        count
    }
}
