//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `Engine::sql` driver: parse SQL via `uqa_sql::compile`, lower each
//! statement onto the engine's mutation / search APIs, and roll the
//! result rows into a [`SqlResult`].
//!
//! Phase 5 covers the quickstart slice: `CREATE TABLE` (with `VECTOR(N)`
//! columns), `CREATE INDEX ... USING gin (...)` (recorded as an FTS
//! field), `INSERT ... VALUES`, and `SELECT` with `text_match`,
//! `knn_match`, and `fuse_log_odds` calls in `WHERE`. Statements outside
//! this surface return [`uqa_sql::SqlError::Unsupported`] cleanly.

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

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::{
    ColumnDef as SqlColumnDef, ColumnType, CreateIndex, CreateTable, Cte, DeleteStmt, Expr,
    FromClause, InsertStmt, JoinKind, OrderBy, Projection, SelectStmt, SetOpKind, Statement,
    UpdateStmt, WindowSpec,
};
use uqa_sql::expr::{eval, value_to_vector, EvalContext};
use uqa_sql::registry::{lookup, FunctionKind};
use uqa_sql::{compile, ResultRow, SqlError, SqlParam, SqlResult};
use uqa_storage::document_store::Document;

use crate::{Engine, HybridSearchParams, ScoredEntry};

const SCORE_COLUMN: &str = "_score";

pub fn execute(engine: &Engine, sql: &str, params: &[SqlParam]) -> Result<SqlResult, SqlError> {
    let stmts = compile(sql)?;
    if stmts.is_empty() {
        return Ok(SqlResult::empty());
    }
    let mut last = SqlResult::empty();
    for stmt in stmts {
        last = run_stmt(engine, stmt, params)?;
    }
    Ok(last)
}

fn run_stmt(engine: &Engine, stmt: Statement, params: &[SqlParam]) -> Result<SqlResult, SqlError> {
    match stmt {
        Statement::CreateTable(c) => run_create_table(engine, c),
        Statement::CreateIndex(c) => run_create_index(engine, c),
        Statement::Insert(i) => run_insert(engine, i, params),
        Statement::Select(s) => run_select(engine, *s, params),
        Statement::Update(u) => run_update(engine, u, params),
        Statement::Delete(d) => run_delete(engine, d, params),
    }
}

fn run_update(
    engine: &Engine,
    stmt: UpdateStmt,
    params: &[SqlParam],
) -> Result<SqlResult, SqlError> {
    let mut affected = 0u64;
    for doc_id in engine.table_doc_ids(&stmt.table) {
        let mut doc = engine
            .get_document(&stmt.table, doc_id)
            .ok_or_else(|| SqlError::Internal("missing document during UPDATE".into()))?;
        if let Some(filter) = stmt.r#where.as_ref() {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&doc), params);
            if !uqa_sql::expr::truthy(&uqa_sql::expr::eval(filter, &ctx)?) {
                continue;
            }
        }
        let mut new_vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        for (col, expr) in &stmt.assignments {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&doc), params);
            let value = uqa_sql::expr::eval(expr, &ctx)?;
            if let Ok(vec) = uqa_sql::expr::value_to_vector(&value) {
                new_vectors.insert(col.clone(), vec);
            }
            doc.insert(col.clone(), value);
        }
        engine.add_document_with_vectors(&stmt.table, doc_id, doc, new_vectors);
        affected += 1;
    }
    Ok(SqlResult::from_affected(affected))
}

fn run_delete(
    engine: &Engine,
    stmt: DeleteStmt,
    params: &[SqlParam],
) -> Result<SqlResult, SqlError> {
    let mut affected = 0u64;
    let to_delete: Vec<uqa_core::DocId> = engine
        .table_doc_ids(&stmt.table)
        .into_iter()
        .filter(|&doc_id| {
            let Some(doc) = engine.get_document(&stmt.table, doc_id) else {
                return false;
            };
            match stmt.r#where.as_ref() {
                None => true,
                Some(filter) => {
                    let ctx = uqa_sql::expr::EvalContext::new(Some(&doc), params);
                    matches!(
                        uqa_sql::expr::eval(filter, &ctx).map(|v| uqa_sql::expr::truthy(&v)),
                        Ok(true)
                    )
                }
            }
        })
        .collect();
    for doc_id in to_delete {
        engine.delete_document(&stmt.table, doc_id);
        affected += 1;
    }
    Ok(SqlResult::from_affected(affected))
}

// -------------------------------------------------------------------------
// DDL
// -------------------------------------------------------------------------

fn run_create_table(engine: &Engine, c: CreateTable) -> Result<SqlResult, SqlError> {
    let mut fts_fields = Vec::new();
    let mut vector_fields: Vec<(String, u32)> = Vec::new();
    for col in &c.columns {
        match &col.ty {
            ColumnType::Text => fts_fields.push(col.name.clone()),
            ColumnType::Vector(dim) => vector_fields.push((col.name.clone(), *dim)),
            _ => {}
        }
    }
    engine.create_default_table(c.name.clone(), fts_fields);
    for (field, dim) in vector_fields {
        engine.create_vector_field(&c.name, field, dim);
    }
    let _ = column_names(&c.columns); // sanity, used by future EXPLAIN
    Ok(SqlResult::empty())
}

fn column_names(cols: &[SqlColumnDef]) -> Vec<String> {
    cols.iter().map(|c| c.name.clone()).collect()
}

fn run_create_index(_engine: &Engine, _c: CreateIndex) -> Result<SqlResult, SqlError> {
    // FTS fields are derived from TEXT columns at CREATE TABLE time, so
    // CREATE INDEX is informational only in Phase 5. We accept any
    // access method (gin, btree, ivf, rtree) and treat it as a no-op.
    Ok(SqlResult::empty())
}

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------

fn run_insert(
    engine: &Engine,
    stmt: InsertStmt,
    params: &[SqlParam],
) -> Result<SqlResult, SqlError> {
    if stmt.columns.is_empty() {
        return Err(SqlError::Unsupported(
            "INSERT without explicit column list".into(),
        ));
    }
    let id_index = stmt
        .columns
        .iter()
        .position(|c| c == "id")
        .ok_or_else(|| SqlError::Unsupported("INSERT requires an `id` column".into()))?;

    let mut affected = 0u64;
    let ctx = EvalContext::new(None, params);
    for row in &stmt.rows {
        if row.len() != stmt.columns.len() {
            return Err(SqlError::Internal(format!(
                "row width {} != column count {}",
                row.len(),
                stmt.columns.len()
            )));
        }
        let mut document = Document::new();
        let mut vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        let mut doc_id: Option<u64> = None;
        for (i, col) in stmt.columns.iter().enumerate() {
            let v = eval(&row[i], &ctx)?;
            if i == id_index {
                doc_id = match &v {
                    Value::Int(n) if *n >= 0 => Some(*n as u64),
                    other => {
                        return Err(SqlError::TypeMismatch(format!(
                            "id must be a non-negative integer, got {other:?}"
                        )));
                    }
                };
            }
            // Heuristic: rows whose value compiles to a list go to the
            // vector index; the document store keeps the raw value too
            // so the projection can read it back later.
            if let Ok(vec) = value_to_vector(&v) {
                vectors.insert(col.clone(), vec);
            }
            document.insert(col.clone(), v);
        }
        let doc_id = doc_id.ok_or_else(|| SqlError::Internal("INSERT missing id value".into()))?;
        engine.add_document_with_vectors(
            &stmt.table,
            doc_id,
            document,
            vectors_to_field_map(vectors),
        );
        affected += 1;
    }
    Ok(SqlResult::from_affected(affected))
}

fn vectors_to_field_map(
    vectors: BTreeMap<String, Vec<f32>>,
) -> BTreeMap<uqa_core::FieldName, Vec<f32>> {
    vectors
}

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

fn run_select(
    engine: &Engine,
    stmt: SelectStmt,
    params: &[SqlParam],
) -> Result<SqlResult, SqlError> {
    if !stmt.with.is_empty() || stmt.set_op.is_some() {
        let mut ctes: BTreeMap<String, Vec<ResultRow>> = BTreeMap::new();
        return execute_select(engine, &stmt, params, &mut ctes);
    }

    let from = stmt
        .from
        .as_ref()
        .ok_or_else(|| SqlError::Unsupported("SELECT without FROM".into()))?;

    // Single-table FROM with no alias and no window function keeps the
    // search-aware fast path. JOIN shapes and window queries drop into
    // the multi-table executor that builds row tuples up-front and
    // filters them via the expression evaluator.
    if let FromClause::Table { name, alias } = from {
        if alias.is_none() && !has_window(&stmt.projections) {
            return run_single_table_select(engine, name, &stmt, params);
        }
    }

    run_joined_select(engine, from, &stmt, params)
}

/// Execute a SELECT that may carry CTEs and/or set ops, returning the
/// final result. CTEs are materialized into the `ctes` map first so the
/// FROM clause can resolve references to them.
fn execute_select(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SqlParam],
    ctes: &mut BTreeMap<String, Vec<ResultRow>>,
) -> Result<SqlResult, SqlError> {
    materialize_ctes(engine, &stmt.with, params, ctes)?;

    let lhs = run_query_block(engine, stmt, params, ctes)?;

    let Some(set_op) = stmt.set_op.as_ref() else {
        return Ok(lhs);
    };
    let rhs = execute_select(engine, &set_op.right, params, ctes)?;
    let combined = match (set_op.kind, set_op.all) {
        (SetOpKind::Union, true) => {
            let mut rows = lhs.rows;
            rows.extend(rhs.rows);
            SqlResult::from_rows(lhs.columns, rows)
        }
        (SetOpKind::Union, false) => {
            let mut rows = lhs.rows;
            rows.extend(rhs.rows);
            rows.dedup();
            SqlResult::from_rows(lhs.columns, rows)
        }
        (SetOpKind::Intersect, _) => {
            let mut rows: Vec<ResultRow> = lhs
                .rows
                .into_iter()
                .filter(|r| rhs.rows.iter().any(|s| s == r))
                .collect();
            if !set_op.all {
                rows.dedup();
            }
            SqlResult::from_rows(lhs.columns, rows)
        }
        (SetOpKind::Except, _) => {
            let mut rows: Vec<ResultRow> = lhs
                .rows
                .into_iter()
                .filter(|r| !rhs.rows.iter().any(|s| s == r))
                .collect();
            if !set_op.all {
                rows.dedup();
            }
            SqlResult::from_rows(lhs.columns, rows)
        }
    };
    Ok(combined)
}

fn run_query_block(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SqlParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<SqlResult, SqlError> {
    let from = stmt
        .from
        .as_ref()
        .ok_or_else(|| SqlError::Unsupported("SELECT without FROM".into()))?;

    // If the only FROM is a single CTE-backed table with no alias and
    // no window/aggregate, route through the joined path so the CTE
    // rows get the correct qualifier prefixing.
    let mut joined = build_join_rows_with_ctes(engine, from, params, ctes)?;

    if let Some(filter) = stmt.r#where.as_ref() {
        joined.retain(|row| {
            let ctx = uqa_sql::expr::EvalContext::new(Some(row), params);
            uqa_sql::expr::eval(filter, &ctx)
                .map(|v| uqa_sql::expr::truthy(&v))
                .unwrap_or(false)
        });
    }

    if has_aggregate(&stmt.projections) || !stmt.group_by.is_empty() {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(stmt, &joined, params)?;
        let rows = apply_row_order_limit(rows, stmt, params)?;
        return Ok(SqlResult::from_rows(columns, rows));
    }

    if has_window(&stmt.projections) {
        let columns = projection_columns(&stmt.projections);
        let with_windows = compute_window_columns(&stmt.projections, joined, params)?;
        let mut rows: Vec<ResultRow> = with_windows
            .iter()
            .map(|src| project_join_row(src, &stmt.projections, params))
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, params)?;
        return Ok(SqlResult::from_rows(columns, rows));
    }

    let columns = projection_columns(&stmt.projections);
    let mut rows: Vec<ResultRow> = joined
        .iter()
        .map(|src| project_join_row(src, &stmt.projections, params))
        .collect::<Result<_, _>>()?;
    rows = apply_row_order_limit(rows, stmt, params)?;
    Ok(SqlResult::from_rows(columns, rows))
}

fn materialize_ctes(
    engine: &Engine,
    list: &[Cte],
    params: &[SqlParam],
    ctes: &mut BTreeMap<String, Vec<ResultRow>>,
) -> Result<(), SqlError> {
    for cte in list {
        let rows = if cte.recursive {
            materialize_recursive_cte(engine, cte, params, ctes)?
        } else {
            execute_select(engine, &cte.query, params, ctes)?.rows
        };
        ctes.insert(cte.name.clone(), rows);
    }
    Ok(())
}

/// Iterate the recursive CTE: take the anchor (LHS of UNION ALL) as
/// the initial row set, then repeatedly evaluate the recursive step
/// (RHS) with the CTE bound to the *new rows from the previous
/// iteration* (working set), unioning the result back into the total.
/// Caps at 1024 iterations to keep buggy queries from running away.
fn materialize_recursive_cte(
    engine: &Engine,
    cte: &Cte,
    params: &[SqlParam],
    ctes: &mut BTreeMap<String, Vec<ResultRow>>,
) -> Result<Vec<ResultRow>, SqlError> {
    let set_op = cte
        .query
        .set_op
        .as_ref()
        .ok_or_else(|| SqlError::Unsupported("recursive CTE requires UNION ALL".into()))?;
    if set_op.kind != SetOpKind::Union || !set_op.all {
        return Err(SqlError::Unsupported(
            "recursive CTE only supports UNION ALL".into(),
        ));
    }

    // Anchor: the LHS — the same SelectStmt with set_op stripped.
    let mut anchor_stmt = cte.query.as_ref().clone();
    anchor_stmt.set_op = None;
    anchor_stmt.with.clear();
    let anchor_columns = projection_columns(&anchor_stmt.projections);
    let anchor_rows = run_query_block(engine, &anchor_stmt, params, ctes)?.rows;

    let mut all_rows = anchor_rows.clone();
    let mut working = anchor_rows;

    let mut step_stmt = set_op.right.clone();
    step_stmt.with.clear();
    let step_columns = projection_columns(&step_stmt.projections);

    const MAX_ITER: usize = 1024;
    for _ in 0..MAX_ITER {
        if working.is_empty() {
            break;
        }
        // Bind the CTE name to the working set under the anchor's
        // column shape so the recursive step's FROM ... <cte> ... sees
        // the same keys it saw on the prior pass.
        let working_normalized: Vec<ResultRow> = working
            .iter()
            .map(|row| rename_columns(row, &anchor_columns, &anchor_columns))
            .collect();
        ctes.insert(cte.name.clone(), working_normalized);
        let new_rows = run_query_block(engine, &step_stmt, params, ctes)?.rows;
        ctes.remove(&cte.name);

        if new_rows.is_empty() {
            break;
        }
        // Rename the step's positional projection labels to the
        // anchor's so subsequent iterations and the outer SELECT see a
        // consistent shape (anchor names win, mirroring PostgreSQL).
        let renamed: Vec<ResultRow> = new_rows
            .into_iter()
            .map(|row| rename_columns(&row, &step_columns, &anchor_columns))
            .collect();
        all_rows.extend(renamed.clone());
        working = renamed;
    }
    Ok(all_rows)
}

fn rename_columns(row: &ResultRow, src: &[String], dst: &[String]) -> ResultRow {
    let mut out = ResultRow::new();
    for (i, key) in src.iter().enumerate() {
        if let Some(value) = row.get(key) {
            let target = dst.get(i).cloned().unwrap_or_else(|| key.clone());
            out.insert(target, value.clone());
        }
    }
    for (k, v) in row {
        out.entry(k.clone()).or_insert_with(|| v.clone());
    }
    out
}

fn run_single_table_select(
    engine: &Engine,
    table: &str,
    stmt: &SelectStmt,
    params: &[SqlParam],
) -> Result<SqlResult, SqlError> {
    let scored = match stmt.r#where.as_ref() {
        None => engine
            .table_doc_ids(table)
            .into_iter()
            .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
            .collect::<Vec<_>>(),
        Some(Expr::Func { name, args }) if uqa_sql::registry::is_registered(name) => {
            execute_function(engine, table, name, args, params)?
        }
        Some(filter_expr) => filter_table_rows(engine, table, filter_expr, params)?,
    };

    if has_aggregate(&stmt.projections) || !stmt.group_by.is_empty() {
        let columns = projection_columns(&stmt.projections);
        let rows = build_aggregate_rows(engine, table, &scored, stmt, params)?;
        let rows = apply_row_order_limit(rows, stmt, params)?;
        return Ok(SqlResult::from_rows(columns, rows));
    }

    if order_by_references_field(stmt) {
        // ORDER BY references something other than `_score`; defer
        // ordering / skip / limit to the row-level evaluator that can
        // resolve arbitrary expressions against the projected
        // document.
        let columns = projection_columns(&stmt.projections);
        let mut all_rows = build_rows(engine, table, &scored, &stmt.projections, params)?;
        // Bring the underlying document fields into each row so the
        // row evaluator can read columns like `qty` even when the
        // SELECT projection drops them.
        for (entry, row) in scored.iter().zip(all_rows.iter_mut()) {
            if let Some(doc) = engine.get_document(table, entry.doc_id) {
                for (k, v) in doc {
                    row.entry(k).or_insert(v);
                }
            }
        }
        let rows = apply_row_order_limit(all_rows, stmt, params)?;
        // Strip the helper fields to keep the projection honest.
        let projected: Vec<_> = rows
            .into_iter()
            .map(|mut row| {
                row.retain(|k, _| columns.iter().any(|c| c == k));
                row
            })
            .collect();
        return Ok(SqlResult::from_rows(columns, projected));
    }

    let scored = apply_order_limit(scored, stmt);
    let columns = projection_columns(&stmt.projections);
    let rows = build_rows(engine, table, &scored, &stmt.projections, params)?;
    Ok(SqlResult::from_rows(columns, rows))
}

fn order_by_references_field(stmt: &SelectStmt) -> bool {
    stmt.order_by.iter().any(|o| match &o.expr {
        Expr::Column(name) => name != "_score",
        _ => true,
    })
}

/// Multi-table SELECT path. Each input table contributes a row set
/// keyed by `<alias>.<column>` (the alias falls back to the bare table
/// name when no `AS` is given). The same key shape feeds the WHERE
/// expression evaluator, the GROUP BY accumulators, and the projection
/// resolver.
fn run_joined_select(
    engine: &Engine,
    from: &FromClause,
    stmt: &SelectStmt,
    params: &[SqlParam],
) -> Result<SqlResult, SqlError> {
    let mut joined = build_join_rows(engine, from, params)?;

    if let Some(filter) = stmt.r#where.as_ref() {
        joined.retain(|row| {
            let ctx = uqa_sql::expr::EvalContext::new(Some(row), params);
            uqa_sql::expr::eval(filter, &ctx)
                .map(|v| uqa_sql::expr::truthy(&v))
                .unwrap_or(false)
        });
    }

    if has_aggregate(&stmt.projections) || !stmt.group_by.is_empty() {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(stmt, &joined, params)?;
        let rows = apply_row_order_limit(rows, stmt, params)?;
        return Ok(SqlResult::from_rows(columns, rows));
    }

    if has_window(&stmt.projections) {
        let columns = projection_columns(&stmt.projections);
        let with_windows = compute_window_columns(&stmt.projections, joined, params)?;
        let mut rows: Vec<ResultRow> = with_windows
            .iter()
            .map(|src| project_join_row(src, &stmt.projections, params))
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, params)?;
        return Ok(SqlResult::from_rows(columns, rows));
    }

    let columns = projection_columns(&stmt.projections);
    let mut rows: Vec<ResultRow> = joined
        .iter()
        .map(|src| project_join_row(src, &stmt.projections, params))
        .collect::<Result<_, _>>()?;
    rows = apply_row_order_limit(rows, stmt, params)?;
    Ok(SqlResult::from_rows(columns, rows))
}

fn has_window(projections: &[Projection]) -> bool {
    projections
        .iter()
        .any(|p| matches!(p.expr, Expr::WindowCall { .. }))
}

fn compute_window_columns(
    projections: &[Projection],
    rows: Vec<ResultRow>,
    params: &[SqlParam],
) -> Result<Vec<ResultRow>, SqlError> {
    let mut rows = rows;
    let labels = projection_columns(projections);
    for (idx, proj) in projections.iter().enumerate() {
        let Expr::WindowCall { name, args, spec } = &proj.expr else {
            continue;
        };
        let label = labels[idx].clone();
        let values = evaluate_window(name, args, spec, &rows, params)?;
        for (row, value) in rows.iter_mut().zip(values) {
            row.insert(label.clone(), value);
        }
    }
    Ok(rows)
}

fn evaluate_window(
    name: &str,
    args: &[Expr],
    spec: &WindowSpec,
    rows: &[ResultRow],
    params: &[SqlParam],
) -> Result<Vec<Value>, SqlError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut partitions: BTreeMap<Vec<Value>, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params);
        let key: Vec<Value> = spec
            .partition_by
            .iter()
            .map(|e| uqa_sql::expr::eval(e, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        partitions.entry(key).or_default().push(i);
    }
    let mut output = vec![Value::Null; rows.len()];
    let lower = name.to_ascii_lowercase();
    for (_, indices) in partitions {
        let mut indexed: Vec<(usize, Vec<Value>)> = indices
            .into_iter()
            .map(|i| -> Result<_, SqlError> {
                let ctx = uqa_sql::expr::EvalContext::new(Some(&rows[i]), params);
                let key: Vec<Value> = spec
                    .order_by
                    .iter()
                    .map(|o| uqa_sql::expr::eval(&o.expr, &ctx))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((i, key))
            })
            .collect::<Result<Vec<_>, _>>()?;
        indexed.sort_by(|a, b| sort_keys(&a.1, &b.1, &spec.order_by));

        match lower.as_str() {
            "row_number" => {
                for (rank, (orig, _)) in indexed.iter().enumerate() {
                    output[*orig] = Value::Int((rank + 1) as i64);
                }
            }
            "rank" => {
                let mut last_key: Option<Vec<Value>> = None;
                let mut last_rank = 0i64;
                for (i, (orig, key)) in indexed.iter().enumerate() {
                    let rank = if last_key.as_ref() == Some(key) {
                        last_rank
                    } else {
                        last_key = Some(key.clone());
                        last_rank = (i + 1) as i64;
                        last_rank
                    };
                    output[*orig] = Value::Int(rank);
                }
            }
            "dense_rank" => {
                let mut last_key: Option<Vec<Value>> = None;
                let mut last_rank = 0i64;
                for (orig, key) in &indexed {
                    if last_key.as_ref() != Some(key) {
                        last_rank += 1;
                        last_key = Some(key.clone());
                    }
                    output[*orig] = Value::Int(last_rank);
                }
            }
            "lag" | "lead" => {
                let direction: i64 = if lower == "lag" { -1 } else { 1 };
                let target_expr = args.first().ok_or_else(|| SqlError::BadArity {
                    name: lower.clone(),
                    expected: ">=1".into(),
                    actual: 0,
                })?;
                let offset_value = match args.get(1) {
                    None => 1i64,
                    Some(expr) => {
                        let ctx =
                            uqa_sql::expr::EvalContext::new(Some(&rows[indexed[0].0]), params);
                        match uqa_sql::expr::eval(expr, &ctx)? {
                            Value::Int(n) => n,
                            other => {
                                return Err(SqlError::TypeMismatch(format!(
                                    "lag/lead offset must be integer, got {other:?}"
                                )));
                            }
                        }
                    }
                };
                let default_value = match args.get(2) {
                    None => Value::Null,
                    Some(expr) => {
                        let ctx =
                            uqa_sql::expr::EvalContext::new(Some(&rows[indexed[0].0]), params);
                        uqa_sql::expr::eval(expr, &ctx)?
                    }
                };
                for (i, (orig, _)) in indexed.iter().enumerate() {
                    let target_idx = i as i64 + direction * offset_value;
                    let value = if target_idx < 0 || target_idx as usize >= indexed.len() {
                        default_value.clone()
                    } else {
                        let target_orig = indexed[target_idx as usize].0;
                        let ctx = uqa_sql::expr::EvalContext::new(Some(&rows[target_orig]), params);
                        uqa_sql::expr::eval(target_expr, &ctx)?
                    };
                    output[*orig] = value;
                }
            }
            "ntile" => {
                let n = match args.first() {
                    Some(expr) => {
                        let ctx =
                            uqa_sql::expr::EvalContext::new(Some(&rows[indexed[0].0]), params);
                        match uqa_sql::expr::eval(expr, &ctx)? {
                            Value::Int(n) if n > 0 => n,
                            other => {
                                return Err(SqlError::TypeMismatch(format!(
                                    "ntile bucket count must be positive integer, got {other:?}"
                                )));
                            }
                        }
                    }
                    None => {
                        return Err(SqlError::BadArity {
                            name: "ntile".into(),
                            expected: "1".into(),
                            actual: 0,
                        });
                    }
                };
                let len = indexed.len() as i64;
                let base = len / n;
                let extra = len % n;
                let mut bucket = 1i64;
                let mut consumed_in_bucket = 0i64;
                let mut bucket_size = if 1 <= extra { base + 1 } else { base };
                for (orig, _) in &indexed {
                    if bucket_size == 0 {
                        output[*orig] = Value::Int(bucket);
                        bucket += 1;
                        continue;
                    }
                    output[*orig] = Value::Int(bucket);
                    consumed_in_bucket += 1;
                    if consumed_in_bucket == bucket_size {
                        bucket += 1;
                        consumed_in_bucket = 0;
                        bucket_size = if bucket <= extra { base + 1 } else { base };
                    }
                }
            }
            other => {
                return Err(SqlError::UnknownFunction(format!(
                    "window function `{other}` is not supported"
                )));
            }
        }
    }
    Ok(output)
}

fn sort_keys(a: &[Value], b: &[Value], order: &[OrderBy]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (i, (av, bv)) in a.iter().zip(b.iter()).enumerate() {
        let mut cmp = compare_values(av, bv);
        if order.get(i).map_or(false, |o| o.descending) {
            cmp = cmp.reverse();
        }
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    Ordering::Equal
}

fn qualifier_for(name: &str, alias: Option<&str>) -> String {
    alias.unwrap_or(name).to_string()
}

fn load_table_rows(engine: &Engine, table: &str) -> Vec<Document> {
    engine
        .table_doc_ids(table)
        .into_iter()
        .filter_map(|id| engine.get_document(table, id))
        .collect()
}

fn prefix_row(qual: &str, doc: &Document) -> ResultRow {
    let mut out = ResultRow::new();
    for (k, v) in doc {
        out.insert(format!("{qual}.{k}"), v.clone());
    }
    out
}

/// Re-key a row that already has unprefixed column labels onto a new
/// qualifier. Used to plug CTE materializations into the JOIN executor
/// under whatever alias the outer query referenced them by.
fn reprefix_row(qual: &str, row: &ResultRow) -> ResultRow {
    let mut out = ResultRow::new();
    for (k, v) in row {
        // CTE rows are already keyed by their projection labels; lift
        // unqualified labels under the new qualifier so qualified refs
        // (`alias.col`) and unqualified suffix matches both resolve.
        let key = if k.contains('.') {
            // Strip an existing qualifier and re-prefix.
            let (_, col) = k.split_once('.').unwrap_or((qual, k.as_str()));
            format!("{qual}.{col}")
        } else {
            format!("{qual}.{k}")
        };
        out.insert(key, v.clone());
    }
    out
}

fn merge_rows(left: &ResultRow, right: &ResultRow) -> ResultRow {
    let mut out = left.clone();
    for (k, v) in right {
        out.insert(k.clone(), v.clone());
    }
    out
}

fn null_row_for(table: &str, alias: Option<&str>, engine: &Engine) -> ResultRow {
    let qual = qualifier_for(table, alias);
    let mut out = ResultRow::new();
    // Emit NULLs for any column that ever appeared in the table; for an
    // empty table we still know the keys via document_count, but the
    // safe default is just an empty row — a missing key resolves to
    // NULL through Expr::Column / QualifiedColumn lookup anyway.
    let mut sample_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for id in engine.table_doc_ids(table) {
        if let Some(doc) = engine.get_document(table, id) {
            for k in doc.keys() {
                sample_keys.insert(k.clone());
            }
        }
        if sample_keys.len() > 16 {
            break;
        }
    }
    for k in sample_keys {
        out.insert(format!("{qual}.{k}"), Value::Null);
    }
    out
}

fn build_join_rows(
    engine: &Engine,
    from: &FromClause,
    params: &[SqlParam],
) -> Result<Vec<ResultRow>, SqlError> {
    build_join_rows_with_ctes(engine, from, params, &BTreeMap::new())
}

fn build_join_rows_with_ctes(
    engine: &Engine,
    from: &FromClause,
    params: &[SqlParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<Vec<ResultRow>, SqlError> {
    match from {
        FromClause::Table { name, alias } => {
            let qual = qualifier_for(name, alias.as_deref());
            // CTE reference takes precedence over a real table of the
            // same name (matches PostgreSQL semantics).
            if let Some(rows) = ctes.get(name) {
                return Ok(rows.iter().map(|row| reprefix_row(&qual, row)).collect());
            }
            Ok(load_table_rows(engine, name)
                .iter()
                .map(|d| prefix_row(&qual, d))
                .collect())
        }
        FromClause::Join {
            left,
            right,
            kind,
            on,
        } => {
            let left_rows = build_join_rows_with_ctes(engine, left, params, ctes)?;
            let right_rows = build_join_rows_with_ctes(engine, right, params, ctes)?;
            let on_expr = on.as_ref();

            match kind {
                JoinKind::Inner | JoinKind::Cross => {
                    if matches!(kind, JoinKind::Inner) {
                        if let Some(rows) =
                            try_hash_inner_join(&left_rows, &right_rows, on_expr, params)?
                        {
                            return Ok(rows);
                        }
                    }
                    Ok(cross_filter(&left_rows, &right_rows, on_expr, params)?)
                }
                JoinKind::Left => Ok(left_outer(
                    &left_rows,
                    &right_rows,
                    right,
                    on_expr,
                    params,
                    engine,
                )?),
                JoinKind::Right => Ok(left_outer(
                    &right_rows,
                    &left_rows,
                    left,
                    on_expr,
                    params,
                    engine,
                )?),
                JoinKind::Full => Err(SqlError::Unsupported("FULL OUTER JOIN".into())),
            }
        }
    }
}

/// Detect an equijoin shape `<col_a> = <col_b>` and run a hash join.
///
/// Returns `Some(rows)` when the predicate is a clean equality
/// between qualified columns from the two sides. Returns `None` for
/// every other shape; the caller then falls back to the nested-loop
/// cross filter.
fn try_hash_inner_join(
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    on: Option<&Expr>,
    params: &[SqlParam],
) -> Result<Option<Vec<ResultRow>>, SqlError> {
    let Some(Expr::Binary {
        op: uqa_sql::ast::BinaryOp::Equal,
        lhs,
        rhs,
    }) = on
    else {
        return Ok(None);
    };
    let Some((left_key, right_key)) = decide_join_sides(left_rows, right_rows, lhs, rhs, params)
    else {
        return Ok(None);
    };
    // Bucket right rows by their join key.
    let mut buckets: std::collections::BTreeMap<uqa_core::Value, Vec<&ResultRow>> =
        std::collections::BTreeMap::new();
    for row in right_rows {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params);
        if let Ok(v) = uqa_sql::expr::eval(right_key, &ctx) {
            if v != uqa_core::Value::Null {
                buckets.entry(v).or_default().push(row);
            }
        }
    }
    let mut out = Vec::with_capacity(left_rows.len());
    for l in left_rows {
        let ctx = uqa_sql::expr::EvalContext::new(Some(l), params);
        let key = match uqa_sql::expr::eval(left_key, &ctx) {
            Ok(uqa_core::Value::Null) | Err(_) => continue,
            Ok(v) => v,
        };
        if let Some(rows) = buckets.get(&key) {
            for r in rows {
                out.push(merge_rows(l, r));
            }
        }
    }
    Ok(Some(out))
}

/// Pick which expression evaluates over the left side and which over
/// the right by sampling the first row of each side. Returns
/// `(left_key_expr, right_key_expr)` when one direction works,
/// `None` when the predicate isn't separable across sides.
fn decide_join_sides<'a>(
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    lhs: &'a Expr,
    rhs: &'a Expr,
    params: &[SqlParam],
) -> Option<(&'a Expr, &'a Expr)> {
    if left_rows.is_empty() || right_rows.is_empty() {
        return None;
    }
    let l_sample = &left_rows[0];
    let r_sample = &right_rows[0];
    let lhs_on_left = eval_yields_value(l_sample, lhs, params);
    let rhs_on_right = eval_yields_value(r_sample, rhs, params);
    if lhs_on_left && rhs_on_right {
        return Some((lhs, rhs));
    }
    let rhs_on_left = eval_yields_value(l_sample, rhs, params);
    let lhs_on_right = eval_yields_value(r_sample, lhs, params);
    if rhs_on_left && lhs_on_right {
        return Some((rhs, lhs));
    }
    None
}

fn eval_yields_value(row: &ResultRow, expr: &Expr, params: &[SqlParam]) -> bool {
    let ctx = uqa_sql::expr::EvalContext::new(Some(row), params);
    matches!(uqa_sql::expr::eval(expr, &ctx), Ok(v) if v != uqa_core::Value::Null)
}

fn cross_filter(
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    on: Option<&Expr>,
    params: &[SqlParam],
) -> Result<Vec<ResultRow>, SqlError> {
    let mut out = Vec::with_capacity(left_rows.len() * right_rows.len());
    for l in left_rows {
        for r in right_rows {
            let merged = merge_rows(l, r);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx = uqa_sql::expr::EvalContext::new(Some(&merged), params);
                    uqa_sql::expr::truthy(&uqa_sql::expr::eval(expr, &ctx)?)
                }
            };
            if keep {
                out.push(merged);
            }
        }
    }
    Ok(out)
}

fn left_outer(
    outer_rows: &[ResultRow],
    inner_rows: &[ResultRow],
    inner_from: &FromClause,
    on: Option<&Expr>,
    params: &[SqlParam],
    engine: &Engine,
) -> Result<Vec<ResultRow>, SqlError> {
    let mut out = Vec::new();
    for l in outer_rows {
        let mut matched = false;
        for r in inner_rows {
            let merged = merge_rows(l, r);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx = uqa_sql::expr::EvalContext::new(Some(&merged), params);
                    uqa_sql::expr::truthy(&uqa_sql::expr::eval(expr, &ctx)?)
                }
            };
            if keep {
                out.push(merged);
                matched = true;
            }
        }
        if !matched {
            // Pad with NULLs for every column the inner side would
            // have contributed.
            let mut pad = l.clone();
            let mut tables = Vec::new();
            inner_from.collect_tables(&mut tables);
            for (name, alias) in &tables {
                let null_keys = null_row_for(name, alias.as_deref(), engine);
                for (k, v) in null_keys {
                    pad.entry(k).or_insert(v);
                }
            }
            out.push(pad);
        }
    }
    Ok(out)
}

fn project_join_row(
    src: &ResultRow,
    projections: &[Projection],
    params: &[SqlParam],
) -> Result<ResultRow, SqlError> {
    let ctx = uqa_sql::expr::EvalContext::new(Some(src), params);
    let labels = projection_columns(projections);
    let mut out = ResultRow::new();
    for (idx, proj) in projections.iter().enumerate() {
        let label = labels[idx].clone();
        if let Expr::Star = proj.expr {
            for (k, v) in src {
                out.insert(k.clone(), v.clone());
            }
            continue;
        }
        // Window calls are pre-evaluated in `compute_window_columns`
        // and stored on the source row under the projection label;
        // read the cached value through.
        if matches!(proj.expr, Expr::WindowCall { .. }) {
            let value = src.get(&label).cloned().unwrap_or(Value::Null);
            out.insert(label, value);
            continue;
        }
        let value = uqa_sql::expr::eval(&proj.expr, &ctx)?;
        out.insert(label, value);
    }
    Ok(out)
}

fn aggregate_join_rows(
    stmt: &SelectStmt,
    rows: &[ResultRow],
    params: &[SqlParam],
) -> Result<Vec<ResultRow>, SqlError> {
    let agg_targets: Vec<&Projection> = stmt
        .projections
        .iter()
        .filter(|p| is_aggregate(&p.expr))
        .collect();

    let mut groups: BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)> = BTreeMap::new();

    for row in rows {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params);
        let group_values: Vec<Value> = stmt
            .group_by
            .iter()
            .map(|g| uqa_sql::expr::eval(g, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        let bucket = groups.entry(group_values.clone()).or_insert_with(|| {
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                group_values,
            )
        });
        for (i, proj) in agg_targets.iter().enumerate() {
            let Expr::Func { name, args } = &proj.expr else {
                continue;
            };
            let value = match (name.to_ascii_lowercase().as_str(), args.as_slice()) {
                ("count", [Expr::Star]) | ("count", []) => Value::Int(1),
                (_, args) => {
                    let arg = args
                        .first()
                        .ok_or_else(|| SqlError::Internal("aggregate missing arg".into()))?;
                    uqa_sql::expr::eval(arg, &ctx)?
                }
            };
            bucket.0[i].observe(&value);
        }
    }

    if groups.is_empty() && stmt.group_by.is_empty() {
        groups.insert(
            Vec::new(),
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                Vec::new(),
            ),
        );
    }

    let mut out = Vec::with_capacity(groups.len());
    let labels = projection_columns(&stmt.projections);
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let mut agg_idx = 0;
        for (idx, proj) in stmt.projections.iter().enumerate() {
            let label = labels[idx].clone();
            if is_aggregate(&proj.expr) {
                let Expr::Func { name, .. } = &proj.expr else {
                    return Err(SqlError::Internal("aggregate expr lost".into()));
                };
                let acc = &accs[agg_idx];
                agg_idx += 1;
                row.insert(label, aggregate_value(name, acc));
            } else {
                let mut placed = false;
                for (g_expr, g_value) in stmt.group_by.iter().zip(&group_values) {
                    if exprs_match(&proj.expr, g_expr) {
                        row.insert(label.clone(), g_value.clone());
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    return Err(SqlError::Unsupported(format!(
                        "non-aggregated projection `{label}` must appear in GROUP BY"
                    )));
                }
            }
        }
        out.push(row);
    }
    Ok(out)
}

fn exprs_match(lhs: &Expr, rhs: &Expr) -> bool {
    match (lhs, rhs) {
        (Expr::Column(a), Expr::Column(b)) => a == b,
        (
            Expr::QualifiedColumn {
                qualifier: aq,
                column: ac,
            },
            Expr::QualifiedColumn {
                qualifier: bq,
                column: bc,
            },
        ) => aq == bq && ac == bc,
        (Expr::Column(c), Expr::QualifiedColumn { column, .. })
        | (Expr::QualifiedColumn { column, .. }, Expr::Column(c)) => c == column,
        _ => false,
    }
}

fn filter_table_rows(
    engine: &Engine,
    table: &str,
    filter: &Expr,
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    let mut out = Vec::new();
    for doc_id in engine.table_doc_ids(table) {
        let document = engine.get_document(table, doc_id).unwrap_or_default();
        let ctx = uqa_sql::expr::EvalContext::new(Some(&document), params);
        let v = uqa_sql::expr::eval(filter, &ctx)?;
        if uqa_sql::expr::truthy(&v) {
            out.push(ScoredEntry { doc_id, score: 0.0 });
        }
    }
    Ok(out)
}

fn has_aggregate(projections: &[Projection]) -> bool {
    projections.iter().any(|p| is_aggregate(&p.expr))
}

fn is_aggregate(expr: &Expr) -> bool {
    matches!(expr, Expr::Func { name, .. } if matches!(
        name.to_ascii_lowercase().as_str(),
        "count" | "sum" | "avg" | "min" | "max"
    ))
}

#[derive(Default)]
struct AggregateAccumulator {
    count: u64,
    sum: f64,
    min: Option<Value>,
    max: Option<Value>,
}

impl AggregateAccumulator {
    fn observe(&mut self, value: &Value) {
        if matches!(value, Value::Null) {
            return;
        }
        self.count += 1;
        if let Ok(f) = value_as_f64(value) {
            self.sum += f;
        }
        match &self.min {
            Some(cur) if !value_lt(value, cur) => {}
            _ => self.min = Some(value.clone()),
        }
        match &self.max {
            Some(cur) if !value_gt(value, cur) => {}
            _ => self.max = Some(value.clone()),
        }
    }
}

fn value_as_f64(v: &Value) -> Result<f64, SqlError> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        other => Err(SqlError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

fn value_lt(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x < y,
        (Value::Float(x), Value::Float(y)) => x < y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) < *y,
        (Value::Float(x), Value::Int(y)) => *x < (*y as f64),
        (Value::Str(x), Value::Str(y)) => x < y,
        _ => false,
    }
}

fn value_gt(a: &Value, b: &Value) -> bool {
    value_lt(b, a)
}

fn build_aggregate_rows(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    stmt: &SelectStmt,
    params: &[SqlParam],
) -> Result<Vec<ResultRow>, SqlError> {
    // group_key -> per-aggregate accumulator vector + the raw group key
    // values used to project the GROUP BY columns.
    let mut groups: BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)> = BTreeMap::new();
    let agg_targets: Vec<&Projection> = stmt
        .projections
        .iter()
        .filter(|p| is_aggregate(&p.expr))
        .collect();

    for entry in scored {
        let document = engine.get_document(table, entry.doc_id).unwrap_or_default();
        let ctx = uqa_sql::expr::EvalContext::new(Some(&document), params);
        let group_values: Vec<Value> = stmt
            .group_by
            .iter()
            .map(|g| uqa_sql::expr::eval(g, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        let bucket = groups.entry(group_values.clone()).or_insert_with(|| {
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                group_values,
            )
        });
        for (i, proj) in agg_targets.iter().enumerate() {
            let Expr::Func { name, args } = &proj.expr else {
                continue;
            };
            let value = match (name.to_ascii_lowercase().as_str(), args.as_slice()) {
                ("count", [Expr::Star]) | ("count", []) => Value::Int(1),
                (_, args) => {
                    let arg = args
                        .first()
                        .ok_or_else(|| SqlError::Internal("aggregate missing arg".into()))?;
                    uqa_sql::expr::eval(arg, &ctx)?
                }
            };
            bucket.0[i].observe(&value);
        }
    }

    if groups.is_empty() && stmt.group_by.is_empty() {
        // SELECT count(*) FROM t with no rows still produces a row of
        // zeros so downstream consumers see a stable shape.
        groups.insert(
            Vec::new(),
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                Vec::new(),
            ),
        );
    }

    let mut rows: Vec<ResultRow> = Vec::with_capacity(groups.len());
    let labels = projection_columns(&stmt.projections);
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let mut agg_idx = 0;
        for (idx, proj) in stmt.projections.iter().enumerate() {
            let label = labels[idx].clone();
            if is_aggregate(&proj.expr) {
                let Expr::Func { name, .. } = &proj.expr else {
                    return Err(SqlError::Internal("aggregate expr lost".into()));
                };
                let acc = &accs[agg_idx];
                agg_idx += 1;
                row.insert(label, aggregate_value(name, acc));
            } else if let Expr::Column(col) = &proj.expr {
                let mut placed = false;
                for (g_expr, g_value) in stmt.group_by.iter().zip(&group_values) {
                    if let Expr::Column(g_col) = g_expr {
                        if g_col == col {
                            row.insert(label.clone(), g_value.clone());
                            placed = true;
                            break;
                        }
                    }
                }
                if !placed {
                    return Err(SqlError::Unsupported(format!(
                        "non-aggregated projection `{col}` must appear in GROUP BY"
                    )));
                }
            } else {
                return Err(SqlError::Unsupported(
                    "complex non-aggregate projections in GROUP BY are not supported".into(),
                ));
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

fn aggregate_value(name: &str, acc: &AggregateAccumulator) -> Value {
    match name.to_ascii_lowercase().as_str() {
        "count" => Value::Int(acc.count as i64),
        "sum" => Value::Float(acc.sum),
        "avg" => {
            if acc.count == 0 {
                Value::Null
            } else {
                Value::Float(acc.sum / acc.count as f64)
            }
        }
        "min" => acc.min.clone().unwrap_or(Value::Null),
        "max" => acc.max.clone().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// Compute a projection's output column name. The position is folded
/// into the fallback so two unaliased expressions in the same SELECT
/// don't collide on a single key.
fn projection_label_at(proj: &Projection, position: usize) -> String {
    if let Some(a) = &proj.alias {
        return a.clone();
    }
    match &proj.expr {
        Expr::Column(c) => c.clone(),
        Expr::QualifiedColumn { column, .. } => column.clone(),
        Expr::Star => "*".into(),
        Expr::Func { name, .. } => name.clone(),
        _ => format!("expr_{position}"),
    }
}

fn apply_row_order_limit(
    mut rows: Vec<ResultRow>,
    stmt: &SelectStmt,
    params: &[SqlParam],
) -> Result<Vec<ResultRow>, SqlError> {
    if !stmt.order_by.is_empty() {
        let order = stmt.order_by.clone();
        let mut keyed: Vec<(Vec<Value>, ResultRow)> = rows
            .into_iter()
            .map(|row| -> Result<_, SqlError> {
                let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params);
                let key: Vec<Value> = order
                    .iter()
                    .map(|o| uqa_sql::expr::eval(&o.expr, &ctx))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((key, row))
            })
            .collect::<Result<Vec<_>, _>>()?;
        keyed.sort_by(|a, b| {
            for (i, (av, bv)) in a.0.iter().zip(b.0.iter()).enumerate() {
                let cmp = compare_values(av, bv);
                let cmp = if order.get(i).map_or(false, |o| o.descending) {
                    cmp.reverse()
                } else {
                    cmp
                };
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            std::cmp::Ordering::Equal
        });
        rows = keyed.into_iter().map(|(_, r)| r).collect();
    }
    if let Some(offset) = stmt.offset {
        let off = offset as usize;
        if off >= rows.len() {
            rows.clear();
        } else {
            rows.drain(0..off);
        }
    }
    if let Some(limit) = stmt.limit {
        rows.truncate(limit as usize);
    }
    Ok(rows)
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

fn execute_function(
    engine: &Engine,
    table: &str,
    name: &str,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    let kind = lookup(name).ok_or_else(|| SqlError::UnknownFunction(name.to_string()))?;
    match kind {
        FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
            run_text_match(engine, table, args, params)
        }
        FunctionKind::KnnMatch => run_knn_match(engine, table, args, params),
        FunctionKind::FuseLogOdds => run_fuse_log_odds(engine, table, args, params),
        FunctionKind::GraphPagerank => run_graph_pagerank(engine, args, params),
        FunctionKind::GraphTraverse => run_graph_traverse(engine, args, params),
        FunctionKind::GraphNeighbors => run_graph_neighbors(engine, args, params),
        FunctionKind::MultiFieldMatch => run_multi_field_match(engine, table, args, params),
        FunctionKind::StagedRetrieval => run_staged_retrieval(engine, table, args, params),
        FunctionKind::DeepPredict => run_deep_predict(engine, args, params),
    }
}

fn run_deep_predict(
    engine: &Engine,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.len() != 1 {
        return Err(SqlError::BadArity {
            name: "deep_predict".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params);
    let name = match eval(&args[0], &ctx)? {
        Value::Str(s) => s,
        other => {
            return Err(SqlError::TypeMismatch(format!(
                "deep_predict.model must be a string, got {other:?}"
            )));
        }
    };
    let scores = engine
        .deep_predict(&name)
        .ok_or_else(|| SqlError::Unsupported(format!("unknown model {name:?}")))?;
    Ok(scores
        .into_iter()
        .map(|(doc_id, score)| ScoredEntry { doc_id, score })
        .collect())
}

fn run_staged_retrieval(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.is_empty() || args.len() % 3 != 0 {
        return Err(SqlError::BadArity {
            name: "staged_retrieval".into(),
            expected: ">= 3, multiple of 3 (field, query, top_k)".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params);
    let mode = crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params::default());
    let n_stages = args.len() / 3;
    let mut current: Option<Vec<ScoredEntry>> = None;
    for i in 0..n_stages {
        let field = expect_column_name(&args[3 * i], "staged_retrieval.field")?;
        let q = match eval(&args[3 * i + 1], &ctx)? {
            Value::Str(s) => s,
            other => {
                return Err(SqlError::TypeMismatch(format!(
                    "staged_retrieval query must be string, got {other:?}"
                )));
            }
        };
        let top_k = expect_usize(&args[3 * i + 2], "staged_retrieval.top_k", &ctx)?;
        let mut scored = engine.search(table, &field, &q, &mode, usize::MAX);
        if let Some(prior) = &current {
            let prior_ids: std::collections::BTreeSet<u64> =
                prior.iter().map(|e| e.doc_id).collect();
            scored.retain(|e| prior_ids.contains(&e.doc_id));
        }
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        scored.sort_by_key(|e| e.doc_id);
        current = Some(scored);
    }
    Ok(current.unwrap_or_default())
}

fn run_multi_field_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.is_empty() || args.len() % 2 != 0 {
        return Err(SqlError::BadArity {
            name: "multi_field_match".into(),
            expected: "even, >= 2 (alternating field, query)".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params);
    // Run a text_match per (field, query) pair, accumulate per-doc
    // probability vectors with 0.5 prior for missing fields, then fuse
    // via log-odds conjunction with uniform weights.
    let n_fields = args.len() / 2;
    let mut per_doc: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    for i in 0..n_fields {
        let field = expect_column_name(&args[2 * i], "multi_field_match.field")?;
        let q = match eval(&args[2 * i + 1], &ctx)? {
            Value::Str(s) => s,
            other => {
                return Err(SqlError::TypeMismatch(format!(
                    "multi_field_match query must be string, got {other:?}"
                )));
            }
        };
        let mode = crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params::default());
        let scored = engine.search(table, &field, &q, &mode, usize::MAX);
        for entry in scored {
            let slot = per_doc
                .entry(entry.doc_id)
                .or_insert_with(|| vec![0.5; n_fields]);
            slot[i] = entry.score;
        }
    }
    // Pad missing slots so every doc has a full vector.
    for slot in per_doc.values_mut() {
        if slot.len() < n_fields {
            slot.resize(n_fields, 0.5);
        }
    }
    let weights = vec![1.0 / n_fields as f64; n_fields];
    let mut out: Vec<ScoredEntry> = per_doc
        .into_iter()
        .map(|(doc_id, probs)| {
            let fused = if probs.len() == 1 {
                probs[0]
            } else {
                uqa_scoring::prob::log_odds_conjunction_weighted(&probs, &weights, 0.0)
                    .unwrap_or(0.5)
            };
            ScoredEntry {
                doc_id,
                score: fused,
            }
        })
        .collect();
    out.sort_by_key(|e| e.doc_id);
    Ok(out)
}

fn run_graph_pagerank(
    engine: &Engine,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.len() != 1 {
        return Err(SqlError::BadArity {
            name: "graph_pagerank".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params);
    let name = expect_string(&args[0], "graph_pagerank.graph", &ctx)?;
    let entries = engine
        .graph_with(&name, |store| {
            uqa_graph::PageRank::new(&name)
                .execute(store)
                .inner()
                .entries()
                .iter()
                .map(|e| ScoredEntry {
                    doc_id: e.doc_id,
                    score: e.payload.score,
                })
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| SqlError::Unsupported(format!("unknown graph {name:?}")))?;
    Ok(entries)
}

fn run_graph_traverse(
    engine: &Engine,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.len() != 4 {
        return Err(SqlError::BadArity {
            name: "graph_traverse".into(),
            expected: "4".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params);
    let name = expect_string(&args[0], "graph_traverse.graph", &ctx)?;
    let start = expect_u64(&args[1], "graph_traverse.start", &ctx)?;
    let label = expect_optional_string(&args[2], "graph_traverse.label", &ctx)?;
    let max_hops = expect_u32(&args[3], "graph_traverse.max_hops", &ctx)?;
    let entries = engine
        .graph_with(&name, |store| {
            let mut traverse = uqa_graph::Traverse::new(start, &name).max_hops(max_hops);
            if let Some(l) = label.as_deref() {
                traverse = traverse.label(l);
            }
            traverse
                .execute(store)
                .inner()
                .entries()
                .iter()
                .map(|e| ScoredEntry {
                    doc_id: e.doc_id,
                    score: e.payload.score,
                })
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| SqlError::Unsupported(format!("unknown graph {name:?}")))?;
    Ok(entries)
}

fn run_graph_neighbors(
    engine: &Engine,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.len() != 4 {
        return Err(SqlError::BadArity {
            name: "graph_neighbors".into(),
            expected: "4".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params);
    let name = expect_string(&args[0], "graph_neighbors.graph", &ctx)?;
    let vertex = expect_u64(&args[1], "graph_neighbors.vertex", &ctx)?;
    let label = expect_optional_string(&args[2], "graph_neighbors.label", &ctx)?;
    let direction_str = expect_string(&args[3], "graph_neighbors.direction", &ctx)?;
    let direction = match direction_str.to_ascii_lowercase().as_str() {
        "out" => uqa_graph::Direction::Out,
        "in" => uqa_graph::Direction::In,
        "both" => uqa_graph::Direction::Both,
        other => {
            return Err(SqlError::TypeMismatch(format!(
                "graph_neighbors.direction must be 'out'/'in'/'both', got {other:?}"
            )));
        }
    };
    let neighbors = engine
        .graph_with(&name, |store| {
            <uqa_graph::MemoryGraphStore as uqa_graph::GraphStore>::neighbors(
                store,
                vertex,
                label.as_deref(),
                direction,
                &name,
            )
        })
        .ok_or_else(|| SqlError::Unsupported(format!("unknown graph {name:?}")))?;
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for nid in neighbors {
        if seen.insert(nid) {
            out.push(ScoredEntry {
                doc_id: nid,
                score: 1.0,
            });
        }
    }
    Ok(out)
}

fn expect_string(expr: &Expr, name: &str, ctx: &EvalContext) -> Result<String, SqlError> {
    match eval(expr, ctx)? {
        Value::Str(s) => Ok(s),
        other => Err(SqlError::TypeMismatch(format!(
            "{name} must be a string, got {other:?}"
        ))),
    }
}

fn expect_optional_string(
    expr: &Expr,
    name: &str,
    ctx: &EvalContext,
) -> Result<Option<String>, SqlError> {
    match eval(expr, ctx)? {
        Value::Null => Ok(None),
        Value::Str(s) if s.is_empty() => Ok(None),
        Value::Str(s) => Ok(Some(s)),
        other => Err(SqlError::TypeMismatch(format!(
            "{name} must be a string or NULL, got {other:?}"
        ))),
    }
}

fn expect_u64(expr: &Expr, name: &str, ctx: &EvalContext) -> Result<u64, SqlError> {
    match eval(expr, ctx)? {
        Value::Int(n) if n >= 0 => Ok(n as u64),
        other => Err(SqlError::TypeMismatch(format!(
            "{name} must be a non-negative integer, got {other:?}"
        ))),
    }
}

fn expect_u32(expr: &Expr, name: &str, ctx: &EvalContext) -> Result<u32, SqlError> {
    let max_u32_as_i64: i64 = i64::from(u32::MAX);
    match eval(expr, ctx)? {
        Value::Int(n) if (0..=max_u32_as_i64).contains(&n) => Ok(n as u32),
        other => Err(SqlError::TypeMismatch(format!(
            "{name} must fit in u32, got {other:?}"
        ))),
    }
}

fn run_text_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.len() != 2 {
        return Err(SqlError::BadArity {
            name: "text_match".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let field = expect_column_name(&args[0], "text_match.field")?;
    let ctx = EvalContext::new(None, params);
    let query_value = eval(&args[1], &ctx)?;
    let query = match query_value {
        Value::Str(s) => s,
        other => {
            return Err(SqlError::TypeMismatch(format!(
                "text_match query must be a string, got {other:?}"
            )));
        }
    };
    let mode = crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params::default());
    Ok(engine.search(table, &field, &query, &mode, usize::MAX))
}

fn run_knn_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.len() != 3 {
        return Err(SqlError::BadArity {
            name: "knn_match".into(),
            expected: "3".into(),
            actual: args.len(),
        });
    }
    let field = expect_column_name(&args[0], "knn_match.field")?;
    let ctx = EvalContext::new(None, params);
    let vec_value = eval(&args[1], &ctx)?;
    let query_vector = value_to_vector(&vec_value)?;
    let k = expect_usize(&args[2], "knn_match.k", &ctx)?;
    Ok(engine.knn_search(table, &field, query_vector, k))
}

fn run_fuse_log_odds(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.len() < 2 {
        return Err(SqlError::BadArity {
            name: "fuse_log_odds".into(),
            expected: ">=2".into(),
            actual: args.len(),
        });
    }
    // Phase 5 supports the canonical hybrid call shape:
    // `fuse_log_odds(text_match(field, q), knn_match(vec_field, $1, k))`.
    let mut text_field: Option<String> = None;
    let mut text_query: Option<String> = None;
    let mut vector_field: Option<String> = None;
    let mut query_vector: Option<Vec<f32>> = None;
    let mut knn_pool: usize = 10;
    let ctx = EvalContext::new(None, params);
    for arg in args {
        match arg {
            Expr::Func { name, args: inner } => {
                let kind = lookup(name).ok_or_else(|| SqlError::UnknownFunction(name.clone()))?;
                match kind {
                    FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
                        if inner.len() != 2 {
                            return Err(SqlError::BadArity {
                                name: name.clone(),
                                expected: "2".into(),
                                actual: inner.len(),
                            });
                        }
                        text_field = Some(expect_column_name(&inner[0], "text_match.field")?);
                        let q = eval(&inner[1], &ctx)?;
                        text_query = match q {
                            Value::Str(s) => Some(s),
                            other => {
                                return Err(SqlError::TypeMismatch(format!(
                                    "text_match query must be string, got {other:?}"
                                )));
                            }
                        };
                    }
                    FunctionKind::KnnMatch => {
                        if inner.len() != 3 {
                            return Err(SqlError::BadArity {
                                name: name.clone(),
                                expected: "3".into(),
                                actual: inner.len(),
                            });
                        }
                        vector_field = Some(expect_column_name(&inner[0], "knn_match.field")?);
                        query_vector = Some(value_to_vector(&eval(&inner[1], &ctx)?)?);
                        knn_pool = expect_usize(&inner[2], "knn_match.k", &ctx)?;
                    }
                    FunctionKind::FuseLogOdds => {
                        return Err(SqlError::Unsupported(
                            "nested fuse_log_odds is not supported".into(),
                        ));
                    }
                    FunctionKind::GraphPagerank
                    | FunctionKind::GraphTraverse
                    | FunctionKind::GraphNeighbors
                    | FunctionKind::MultiFieldMatch
                    | FunctionKind::StagedRetrieval
                    | FunctionKind::DeepPredict => {
                        return Err(SqlError::Unsupported(format!(
                            "function {name} cannot be nested under fuse_log_odds"
                        )));
                    }
                }
            }
            other => {
                return Err(SqlError::Unsupported(format!(
                    "fuse_log_odds argument must be a function call, got {other:?}"
                )));
            }
        }
    }
    let text_field = text_field
        .ok_or_else(|| SqlError::Unsupported("fuse_log_odds requires a text_match arm".into()))?;
    let text_query = text_query.unwrap_or_default();
    let vector_field = vector_field
        .ok_or_else(|| SqlError::Unsupported("fuse_log_odds requires a knn_match arm".into()))?;
    let query_vector =
        query_vector.ok_or_else(|| SqlError::Internal("missing knn_match vector".into()))?;

    Ok(engine.hybrid_search(&HybridSearchParams {
        table,
        text_field: &text_field,
        text_query: &text_query,
        vector_field: &vector_field,
        query_vector,
        knn_pool,
        alpha: 0.5,
        top_k: usize::MAX,
    }))
}

fn expect_column_name(expr: &Expr, label: &str) -> Result<String, SqlError> {
    match expr {
        Expr::Column(name) => Ok(name.clone()),
        other => Err(SqlError::TypeMismatch(format!(
            "{label} must be a column reference, got {other:?}"
        ))),
    }
}

fn expect_usize(expr: &Expr, label: &str, ctx: &EvalContext<'_>) -> Result<usize, SqlError> {
    let v = eval(expr, ctx)?;
    match v {
        Value::Int(n) if n >= 0 => Ok(n as usize),
        Value::Int(_) => Err(SqlError::TypeMismatch(format!("{label} must be >= 0"))),
        other => Err(SqlError::TypeMismatch(format!(
            "{label} must be an integer, got {other:?}"
        ))),
    }
}

// -------------------------------------------------------------------------
// Output assembly
// -------------------------------------------------------------------------

fn apply_order_limit(mut entries: Vec<ScoredEntry>, stmt: &SelectStmt) -> Vec<ScoredEntry> {
    if !stmt.order_by.is_empty() {
        // Phase 5 only honours `ORDER BY <score-alias|_score>`. Any
        // expression resolves to the entry's score in our limited
        // surface; richer ordering lands in Phase 6 with the proper
        // expression evaluator.
        let descending = stmt.order_by.iter().any(|o| o.descending);
        entries.sort_by(|a, b| {
            let cmp = a
                .score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal);
            if descending { cmp.reverse() } else { cmp }.then_with(|| a.doc_id.cmp(&b.doc_id))
        });
    }
    if let Some(offset) = stmt.offset {
        let off = offset as usize;
        if off >= entries.len() {
            entries.clear();
        } else {
            entries.drain(0..off);
        }
    }
    if let Some(limit) = stmt.limit {
        entries.truncate(limit as usize);
    }
    entries
}

fn projection_columns(projections: &[Projection]) -> Vec<String> {
    let mut out = Vec::with_capacity(projections.len());
    for (idx, proj) in projections.iter().enumerate() {
        let base = projection_label_at(proj, idx);
        let mut label = base.clone();
        let mut suffix = 1usize;
        while out.iter().any(|existing: &String| existing == &label) {
            label = format!("{base}_{suffix}");
            suffix += 1;
        }
        out.push(label);
    }
    out
}

fn build_rows(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    projections: &[Projection],
    params: &[SqlParam],
) -> Result<Vec<ResultRow>, SqlError> {
    let mut rows = Vec::with_capacity(scored.len());
    for entry in scored {
        let mut document = engine.get_document(table, entry.doc_id).unwrap_or_default();
        document.insert(SCORE_COLUMN.into(), Value::Float(entry.score));
        let row = build_projection_row(&document, projections, params)?;
        rows.push(row);
    }
    Ok(rows)
}

fn build_projection_row(
    document: &Document,
    projections: &[Projection],
    params: &[SqlParam],
) -> Result<ResultRow, SqlError> {
    let ctx = EvalContext::new(Some(document), params);
    let labels = projection_columns(projections);
    let mut row = ResultRow::new();
    for (idx, proj) in projections.iter().enumerate() {
        let label = labels[idx].clone();
        if let Expr::Star = proj.expr {
            for (k, v) in document {
                if k.as_str() == SCORE_COLUMN {
                    continue;
                }
                row.insert(k.clone(), v.clone());
            }
            continue;
        }
        let value = eval(&proj.expr, &ctx)?;
        row.insert(label, value);
    }
    Ok(row)
}

impl Engine {
    /// Run an arbitrary SQL statement against the engine. Phase 5
    /// supports the quickstart slice; statements outside the supported
    /// grammar return a structured `Unsupported` error.
    pub fn sql(&self, query: &str, params: &[SqlParam]) -> Result<SqlResult, SqlError> {
        execute(self, query, params)
    }

    /// All doc ids on a table, used by the SELECT path when there is no
    /// WHERE clause.
    pub fn table_doc_ids(&self, table: &str) -> Vec<uqa_core::DocId> {
        let Some(t) = self.tables.read().get(table).cloned() else {
            return Vec::new();
        };
        let ids = t.document_store.read().doc_ids();
        ids
    }
}
