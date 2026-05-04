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
    ColumnDef as SqlColumnDef, ColumnType, CreateIndex, CreateTable, DeleteStmt, Expr, FromClause,
    InsertStmt, JoinKind, OrderBy, Projection, SelectStmt, Statement, UpdateStmt, WindowSpec,
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
        Statement::Select(s) => run_select(engine, s, params),
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

    let scored = apply_order_limit(scored, stmt);
    let columns = projection_columns(&stmt.projections);
    let rows = build_rows(engine, table, &scored, &stmt.projections, params)?;
    Ok(SqlResult::from_rows(columns, rows))
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
    for proj in projections {
        let Expr::WindowCall { name, args, spec } = &proj.expr else {
            continue;
        };
        let label = projection_label(proj);
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
    match from {
        FromClause::Table { name, alias } => {
            let qual = qualifier_for(name, alias.as_deref());
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
            let left_rows = build_join_rows(engine, left, params)?;
            let right_rows = build_join_rows(engine, right, params)?;
            let on_expr = on.as_ref();

            match kind {
                JoinKind::Inner | JoinKind::Cross => {
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
    let mut out = ResultRow::new();
    for proj in projections {
        let label = projection_label(proj);
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
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let mut agg_idx = 0;
        for proj in &stmt.projections {
            let label = projection_label(proj);
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
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let mut agg_idx = 0;
        for proj in &stmt.projections {
            let label = projection_label(proj);
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

fn projection_label(proj: &Projection) -> String {
    match (&proj.alias, &proj.expr) {
        (Some(a), _) => a.clone(),
        (None, Expr::Column(c)) => c.clone(),
        (None, Expr::Star) => "*".into(),
        (None, Expr::Func { name, .. }) => name.clone(),
        (None, _) => "expr".into(),
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
    projections
        .iter()
        .map(|p| match (&p.alias, &p.expr) {
            (Some(a), _) => a.clone(),
            (None, Expr::Column(c)) => c.clone(),
            (None, Expr::Star) => "*".to_string(),
            (None, _) => "expr".to_string(),
        })
        .collect()
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
    let mut row = ResultRow::new();
    for proj in projections {
        let label = match (&proj.alias, &proj.expr) {
            (Some(a), _) => a.clone(),
            (None, Expr::Column(c)) => c.clone(),
            (None, Expr::Star) => "*".into(),
            (None, _) => "expr".into(),
        };
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
