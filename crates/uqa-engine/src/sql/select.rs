//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL SELECT, set-operation, CTE, ordering, and projection execution.

use super::{
    aggregate_join_rows, build_aggregate_rows, build_join_rows, build_join_rows_with_ctes,
    compute_window_columns, engine_func_intercept, eval, execute_function,
    execute_lateral_subquery, execute_mixed_where, expect_column_name, has_aggregate, has_window,
    project_join_row_with_engine, projection_label_at, BTreeMap, BinaryOp, Document, Engine,
    EvalContext, Expr, FromClause, Projection, ResultRow, SQLError, SQLParam, SQLResult,
    ScoredEntry, SelectStmt, SetOpKind, Statement, Value, CTE, DOC_ID_COLUMN, MERGE_ACTION_COLUMN,
    SCORE_COLUMN,
};

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

/// Render the inner statement as an EXPLAIN-style plan result. Mirrors
/// the canonical UQA implementation's `_explain_plan`: returns a single-column `plan` table with
/// one row per line.
pub(super) fn run_explain(
    engine: &Engine,
    body: Statement,
    _params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let plan_text = match &body {
        Statement::Select(stmt) => format_select_plan(stmt),
        other => format!("{other:?}"),
    };
    let mut rows: Vec<ResultRow> = Vec::new();
    for line in plan_text.split('\n') {
        let mut r = ResultRow::new();
        r.insert("plan".to_string(), Value::Str(line.to_string()));
        rows.push(r);
    }
    let _ = engine; // keep the parameter live for future cost extension
    Ok(SQLResult {
        columns: vec!["plan".to_string()],
        rows,
        affected_rows: 0,
    })
}

fn format_select_plan(stmt: &SelectStmt) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "Select");
    if !stmt.projections.is_empty() {
        let _ = writeln!(s, "  projections={}", stmt.projections.len());
    }
    if let Some(from) = &stmt.from {
        let _ = writeln!(s, "  from={from:?}");
    }
    if stmt.r#where.is_some() {
        let _ = writeln!(s, "  where=<expr>");
    }
    if !stmt.group_by.is_empty() {
        let _ = writeln!(s, "  group_by={}", stmt.group_by.len());
    }
    if !stmt.grouping_sets.is_empty() {
        let _ = writeln!(s, "  grouping_sets={}", stmt.grouping_sets.len());
    }
    if !stmt.order_by.is_empty() {
        let _ = writeln!(s, "  order_by={}", stmt.order_by.len());
    }
    if let Some(expr) = stmt.limit.as_ref() {
        let _ = writeln!(s, "  limit={}", explain_int_expr(expr));
    }
    if let Some(expr) = stmt.offset.as_ref() {
        let _ = writeln!(s, "  offset={}", explain_int_expr(expr));
    }
    if stmt.distinct {
        let _ = writeln!(s, "  distinct=true");
    }
    s.trim_end().to_string()
}

pub(super) fn run_select(
    engine: &Engine,
    stmt: SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if !stmt.with.is_empty() || stmt.set_op.is_some() {
        let mut ctes: BTreeMap<String, Vec<ResultRow>> = BTreeMap::new();
        return execute_select(engine, &stmt, params, &mut ctes);
    }

    let Some(from) = stmt.from.as_ref() else {
        // SELECT without FROM -- evaluate the projection list against
        // an empty single-row context. Mirrors the canonical UQA implementation's standalone
        // SELECT 1 / SELECT (SELECT ...).
        return run_select_without_from(engine, &stmt, params);
    };

    // Single-table FROM with no alias and no window function keeps the
    // search-aware fast path. JOIN shapes and window queries drop into
    // the multi-table executor that builds row tuples up-front and
    // filters them via the expression evaluator.
    if let FromClause::Table { name, alias } = from {
        if alias.is_none() && engine.foreign_table(name).is_some() {
            return run_single_foreign_select(engine, name, &stmt, params);
        }
        // Schema-qualified names (information_schema.tables /
        // pg_catalog.pg_*) and CTE references skip the search-aware
        // fast path because they don't correspond to a registered
        // engine table.
        let is_virtual = name.contains('.')
            || (engine.table(name).is_none() && engine.foreign_table(name).is_none());
        if alias.is_none() && !has_window(&stmt.projections) && !is_virtual {
            return run_single_table_select(engine, name, &stmt, params);
        }
    }

    run_joined_select(engine, from, &stmt, params)
}

fn run_select_without_from(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let row = ResultRow::new();
    let projected = build_projection_row(Some(engine), &row, &stmt.projections, params)?;
    let columns = projection_columns(&stmt.projections);
    Ok(SQLResult {
        columns,
        rows: vec![projected],
        affected_rows: 0,
    })
}

/// Execute a SELECT that may carry CTEs and/or set ops, returning the
/// final result. CTEs are materialized into the `ctes` map first so the
/// FROM clause can resolve references to them.
pub(super) fn execute_select(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
    ctes: &mut BTreeMap<String, Vec<ResultRow>>,
) -> Result<SQLResult, SQLError> {
    materialize_ctes(engine, &stmt.with, params, ctes)?;

    // The parent `SelectStmt` carries the LHS branch's own clauses
    // (projections / from / where / group-by / ORDER BY / LIMIT /
    // OFFSET). The set-op-level combined clauses live on
    // `set_op.combined_*`. The LHS branch executes with its own
    // clauses applied; the merged result then takes the combined
    // clauses below.
    let mut lhs = run_query_block(engine, stmt, params, ctes)?;
    if stmt.distinct {
        // SELECT DISTINCT: collapse duplicate output rows.
        // Stable so the relative order of survivors matches PG.
        lhs.rows = distinct_rows_stable(lhs.rows);
    }

    let Some(set_op) = stmt.set_op.as_ref() else {
        return Ok(lhs);
    };
    let rhs = execute_select(engine, &set_op.right, params, ctes)?;
    let mut combined = match (set_op.kind, set_op.all) {
        (SetOpKind::Union, true) => {
            let mut rows = lhs.rows;
            rows.extend(rhs.rows);
            SQLResult::from_rows(lhs.columns, rows)
        }
        (SetOpKind::Union, false) => {
            let mut rows = lhs.rows;
            rows.extend(rhs.rows);
            SQLResult::from_rows(lhs.columns, distinct_rows_stable(rows))
        }
        (SetOpKind::Intersect, _) => {
            let mut rows: Vec<ResultRow> = lhs
                .rows
                .into_iter()
                .filter(|r| rhs.rows.iter().any(|s| s == r))
                .collect();
            if !set_op.all {
                rows = distinct_rows_stable(rows);
            }
            SQLResult::from_rows(lhs.columns, rows)
        }
        (SetOpKind::Except, _) => {
            let mut rows: Vec<ResultRow> = lhs
                .rows
                .into_iter()
                .filter(|r| !rhs.rows.iter().any(|s| s == r))
                .collect();
            if !set_op.all {
                rows = distinct_rows_stable(rows);
            }
            SQLResult::from_rows(lhs.columns, rows)
        }
    };

    // Apply the union-level ORDER BY / LIMIT / OFFSET to the merged
    // set-op result. The parent `stmt`'s own clauses already fired
    // on the LHS branch above; here we use the combined clauses the
    // compiler stashed on the SetOp.
    if !set_op.combined_order_by.is_empty()
        || set_op.combined_limit.is_some()
        || set_op.combined_offset.is_some()
    {
        let synthetic = SelectStmt {
            projections: Vec::new(),
            from: None,
            r#where: None,
            group_by: Vec::new(),
            grouping_sets: Vec::new(),
            having: None,
            order_by: set_op.combined_order_by.clone(),
            limit: set_op.combined_limit.clone(),
            offset: set_op.combined_offset.clone(),
            with: Vec::new(),
            set_op: None,
            distinct: false,
        };
        let columns = combined.columns.clone();
        combined.rows = apply_row_order_limit(combined.rows, &synthetic, engine, params)?;
        combined.columns = columns;
    }
    Ok(combined)
}

fn distinct_rows_stable(rows: Vec<ResultRow>) -> Vec<ResultRow> {
    let mut seen = Vec::with_capacity(rows.len());
    for row in rows {
        if !seen.iter().any(|existing| existing == &row) {
            seen.push(row);
        }
    }
    seen
}

struct ScopedEngineHook<'a> {
    engine: &'a Engine,
    ctes: &'a BTreeMap<String, Vec<ResultRow>>,
}

impl uqa_sql::expr::EngineHook for ScopedEngineHook<'_> {
    fn nextval(&self, name: &str) -> std::result::Result<i64, String> {
        self.engine.nextval(name)
    }

    fn currval(&self, name: &str) -> std::result::Result<i64, String> {
        self.engine.currval(name)
    }

    fn setval(&self, name: &str, value: i64) -> std::result::Result<i64, String> {
        self.engine.setval(name, value)
    }

    fn call_scalar_function(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<Value, SQLError>> {
        self.engine.call_registered_scalar_function(name, args)
    }

    fn run_subquery(
        &self,
        stmt: &uqa_sql::ast::SelectStmt,
        outer_row: Option<&ResultRow>,
        params: &[SQLParam],
    ) -> std::result::Result<(Vec<String>, Vec<ResultRow>), String> {
        run_correlated_subquery(self.engine, stmt, outer_row, params, self.ctes)
            .map(|result| (result.columns, result.rows))
            .map_err(|e| format!("subquery failed: {e}"))
    }
}

pub(crate) fn run_correlated_subquery(
    engine: &Engine,
    stmt: &SelectStmt,
    outer_row: Option<&ResultRow>,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<SQLResult, SQLError> {
    if let Some(row) = outer_row {
        execute_lateral_subquery(engine, stmt, row, params, ctes)
    } else {
        let mut scoped_ctes = ctes.clone();
        execute_select(engine, stmt, params, &mut scoped_ctes)
    }
}

fn run_query_block(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<SQLResult, SQLError> {
    let Some(from) = stmt.from.as_ref() else {
        return run_select_without_from(engine, stmt, params);
    };

    let joined = build_join_rows_with_ctes(engine, from, params, ctes)?;
    let scoped_hook = ScopedEngineHook { engine, ctes };

    // Aggregates and window functions still go through their dedicated
    // routines because they need access to the SQL function registry
    // (e.g. text_match calls in projection lists). Pure projection
    // SELECTs flow through a Volcano sub-pipeline:
    //   TableScan -> [Filter] -> Project -> [Sort] -> [Limit]
    // built on the operators in `uqa-execution` so the planner-driven
    // execution layer is exercised on every projection-only SELECT.
    let filtered = if let Some(filter) = stmt.r#where.as_ref() {
        let mut out: Vec<ResultRow> = Vec::with_capacity(joined.len());
        for row in joined {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(&scoped_hook);
            if uqa_sql::expr::eval(filter, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v)) {
                out.push(row);
            }
        }
        out
    } else {
        joined
    };

    if has_aggregate(engine, &stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &filtered, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    if has_window(&stmt.projections) {
        let columns = projection_columns(&stmt.projections);
        let with_windows = compute_window_columns(engine, &stmt.projections, filtered, params)?;
        let mut rows: Vec<ResultRow> = with_windows
            .iter()
            .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    // Pure projection: use the Volcano Project + Sort + Limit chain.
    let projected = volcano_project_sort_limit(engine, &filtered, stmt, params)?;
    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        from,
    );
    Ok(SQLResult::from_rows(columns, projected))
}

fn expand_from_star_columns(
    columns: Vec<String>,
    projections: &[Projection],
    from: &FromClause,
) -> Vec<String> {
    let has_star = projections.iter().any(|p| matches!(p.expr, Expr::Star));
    if !has_star {
        return columns;
    }
    let source_cols = from_clause_output_columns(from);
    if source_cols.is_empty() {
        return columns;
    }
    let mut out = Vec::with_capacity(columns.len() + source_cols.len());
    for column in columns {
        if column == "*" {
            out.extend(source_cols.iter().cloned());
        } else {
            out.push(column);
        }
    }
    out
}

fn from_clause_output_columns(from: &FromClause) -> Vec<String> {
    match from {
        FromClause::Function {
            name,
            alias,
            column_aliases,
            ..
        } => {
            let cols = if column_aliases.is_empty() {
                vec![name.clone()]
            } else {
                column_aliases.clone()
            };
            qualify_output_columns(alias.as_deref(), cols)
        }
        FromClause::Values {
            rows,
            alias,
            column_aliases,
        } => {
            let cols = if column_aliases.is_empty() {
                let width = rows.first().map_or(0, Vec::len);
                (0..width).map(|idx| format!("column{}", idx + 1)).collect()
            } else {
                column_aliases.clone()
            };
            qualify_output_columns(alias.as_deref(), cols)
        }
        FromClause::Subquery {
            alias,
            column_aliases,
            ..
        } => qualify_output_columns(alias.as_deref(), column_aliases.clone()),
        FromClause::Join { left, right, .. } => {
            let mut cols = from_clause_output_columns(left);
            cols.extend(from_clause_output_columns(right));
            cols
        }
        FromClause::Table { .. } => Vec::new(),
    }
}

fn qualify_output_columns(alias: Option<&str>, columns: Vec<String>) -> Vec<String> {
    match alias {
        Some(a) => columns
            .into_iter()
            .map(|column| format!("{a}.{column}"))
            .collect(),
        None => columns,
    }
}

fn volcano_project_sort_limit(
    engine: &Engine,
    src_rows: &[ResultRow],
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    // Some projection callsites (e.g. `text_match` in the SELECT
    // list) need the engine-side function registry, which the
    // execution-layer Project operator does not understand. Detect
    // those and fall back to the row-by-row engine projector so the
    // contract stays the same for SQL-function-bearing projections.
    let has_engine_funcs = stmt.projections.iter().any(|p| {
        let mut found = false;
        walk_expr(&p.expr, &mut |e| {
            if let Expr::Func { name, .. } = e {
                let lower = name.to_ascii_lowercase();
                if uqa_sql::registry::is_registered(&lower)
                    || engine.has_registered_scalar_function(&lower)
                {
                    found = true;
                }
            }
        });
        found
    });
    let has_star = stmt
        .projections
        .iter()
        .any(|p| matches!(p.expr, Expr::Star));
    // Pre-projection ordering / limiting. PG semantics: ORDER BY can
    // reference columns that the SELECT list drops, so the sort and
    // the limit must happen against the source rows -- the Project
    // step is the *last* node in the pipeline. Output column aliases
    // from the projection list are not addressable here, but the
    // common cases (`ORDER BY <source-column>`, `ORDER BY <const>`)
    // both work because the source row carries every column the
    // FROM relation produced.
    let resolved_offset = resolve_limit_offset(stmt.offset.as_ref(), engine, params, "OFFSET")?;
    let resolved_limit = resolve_limit_offset(stmt.limit.as_ref(), engine, params, "LIMIT")?;

    if has_engine_funcs || has_star {
        let staged = stage_source_rows(
            src_rows,
            stmt,
            engine,
            params,
            resolved_offset,
            resolved_limit,
        )?;
        let rows: Vec<ResultRow> = staged
            .iter()
            .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
            .collect::<Result<_, _>>()?;
        return Ok(rows);
    }

    use uqa_execution::physical::{run_to_rows, ExecError, PhysicalOperator};
    use uqa_execution::relational::{Limit, Project, Sort, SortKey};
    use uqa_execution::scan::TableScan;

    let columns: Vec<String> = src_rows
        .first()
        .map(|r| r.keys().cloned().collect())
        .unwrap_or_default();
    let mut op: Box<dyn PhysicalOperator> =
        Box::new(TableScan::from_rows(columns, src_rows.to_vec()));

    if !stmt.order_by.is_empty() {
        let keys: Vec<SortKey> = stmt
            .order_by
            .iter()
            .map(|o| SortKey {
                expr: o.expr.clone(),
                descending: o.descending,
                nulls_first: o
                    .nulls
                    .map(|n| matches!(n, uqa_sql::ast::NullsOrder::First)),
            })
            .collect();
        op = Box::new(Sort::new(op, keys, params.to_vec()));
    }
    if resolved_offset.is_some() || resolved_limit.is_some() {
        op = Box::new(Limit::new(op, resolved_offset.unwrap_or(0), resolved_limit));
    }

    let labels = projection_columns(&stmt.projections);
    let projections: Vec<(String, Expr)> = stmt
        .projections
        .iter()
        .enumerate()
        .map(|(i, p)| (labels[i].clone(), p.expr.clone()))
        .collect();
    op = Box::new(Project::new(op, projections, params.to_vec()));

    let (_cols, rows) = run_to_rows(op.as_mut()).map_err(|e| match e {
        ExecError::SQL(err) => err,
        ExecError::Other(msg) => SQLError::Internal(msg),
    })?;
    Ok(rows)
}

/// Sort + offset + limit a row set against the source-column namespace
/// (pre-projection). Used by the engine-funcs / star projection path
/// where projection happens row-by-row through the engine after the
/// pipeline has already trimmed the input set.
fn stage_source_rows(
    src_rows: &[ResultRow],
    stmt: &SelectStmt,
    _engine: &Engine,
    params: &[SQLParam],
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_execution::physical::{run_to_rows, PhysicalOperator};
    use uqa_execution::relational::{Limit, Sort, SortKey};
    use uqa_execution::scan::TableScan;

    if stmt.order_by.is_empty() && offset.is_none() && limit.is_none() {
        return Ok(src_rows.to_vec());
    }
    let columns: Vec<String> = src_rows
        .first()
        .map(|r| r.keys().cloned().collect())
        .unwrap_or_default();
    let mut op: Box<dyn PhysicalOperator> =
        Box::new(TableScan::from_rows(columns, src_rows.to_vec()));
    if !stmt.order_by.is_empty() {
        let keys: Vec<SortKey> = stmt
            .order_by
            .iter()
            .map(|o| SortKey {
                expr: o.expr.clone(),
                descending: o.descending,
                nulls_first: o
                    .nulls
                    .map(|n| matches!(n, uqa_sql::ast::NullsOrder::First)),
            })
            .collect();
        op = Box::new(Sort::new(op, keys, params.to_vec()));
    }
    if offset.is_some() || limit.is_some() {
        op = Box::new(Limit::new(op, offset.unwrap_or(0), limit));
    }
    let (_cols, rows) = run_to_rows(op.as_mut()).map_err(|e| match e {
        uqa_execution::physical::ExecError::SQL(err) => err,
        uqa_execution::physical::ExecError::Other(msg) => SQLError::Internal(msg),
    })?;
    Ok(rows)
}

fn walk_expr<F: FnMut(&Expr)>(expr: &Expr, f: &mut F) {
    f(expr);
    match expr {
        Expr::And(parts) | Expr::Or(parts) => {
            for p in parts {
                walk_expr(p, f);
            }
        }
        Expr::Not(inner) => walk_expr(inner, f),
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        Expr::IsNull { expr, .. } => walk_expr(expr, f),
        Expr::Between { expr, low, high } => {
            walk_expr(expr, f);
            walk_expr(low, f);
            walk_expr(high, f);
        }
        Expr::InList { expr, list, .. } => {
            walk_expr(expr, f);
            for p in list {
                walk_expr(p, f);
            }
        }
        Expr::Func { args, .. } | Expr::WindowCall { args, .. } => {
            for p in args {
                walk_expr(p, f);
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(b) = base {
                walk_expr(b, f);
            }
            for (c, r) in when {
                walk_expr(c, f);
                walk_expr(r, f);
            }
            if let Some(e) = else_branch {
                walk_expr(e, f);
            }
        }
        Expr::Cast { expr, .. } => walk_expr(expr, f),
        Expr::Array(items) => {
            for p in items {
                walk_expr(p, f);
            }
        }
        _ => {}
    }
}

pub(super) fn materialize_ctes(
    engine: &Engine,
    list: &[CTE],
    params: &[SQLParam],
    ctes: &mut BTreeMap<String, Vec<ResultRow>>,
) -> Result<(), SQLError> {
    for cte in list {
        let rows = if cte.recursive {
            materialize_recursive_cte(engine, cte, params, ctes)?
        } else {
            let result = execute_select(engine, &cte.query, params, ctes)?;
            apply_cte_column_aliases(result.rows, &result.columns, &cte.columns)
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
    cte: &CTE,
    params: &[SQLParam],
    ctes: &mut BTreeMap<String, Vec<ResultRow>>,
) -> Result<Vec<ResultRow>, SQLError> {
    let set_op = cte
        .query
        .set_op
        .as_ref()
        .ok_or_else(|| SQLError::Unsupported("recursive CTE requires UNION ALL".into()))?;
    if set_op.kind != SetOpKind::Union {
        return Err(SQLError::Unsupported(
            "recursive CTE only supports UNION".into(),
        ));
    }

    // Anchor: the LHS - the same SelectStmt with set_op stripped.
    let mut anchor_stmt = cte.query.as_ref().clone();
    anchor_stmt.set_op = None;
    anchor_stmt.with.clear();
    let source_anchor_columns = projection_columns(&anchor_stmt.projections);
    let anchor_columns = if cte.columns.is_empty() {
        source_anchor_columns.clone()
    } else {
        cte.columns.clone()
    };
    let anchor_rows = run_query_block(engine, &anchor_stmt, params, ctes)?.rows;
    let anchor_rows =
        apply_cte_column_aliases(anchor_rows, &source_anchor_columns, &anchor_columns);

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
        // consistent shape (anchor names win, mirroring `PostgreSQL`).
        let renamed: Vec<ResultRow> = new_rows
            .into_iter()
            .map(|row| rename_columns(&row, &step_columns, &anchor_columns))
            .collect();
        let next = if set_op.all {
            renamed
        } else {
            renamed
                .into_iter()
                .filter(|row| !all_rows.iter().any(|seen| seen == row))
                .collect()
        };
        if next.is_empty() {
            break;
        }
        all_rows.extend(next.clone());
        working = next;
    }
    Ok(all_rows)
}

fn apply_cte_column_aliases(
    rows: Vec<ResultRow>,
    source_columns: &[String],
    aliases: &[String],
) -> Vec<ResultRow> {
    if aliases.is_empty() {
        return rows;
    }
    rows.into_iter()
        .map(|row| rename_columns(&row, source_columns, aliases))
        .collect()
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
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    // Try the operator-tree pipeline first: lower the WHERE clause to
    // an `OperatorTree`, run `QueryOptimizer` (10 algebraic / graph-
    // aware / fusion-reordering passes - compatibility), then execute
    // through `PlanExecutor` against an `EngineDriver`. The bridge
    // returns `None` for shapes the operator IR can't represent
    // (arithmetic across columns, sub-queries, window calls, ...) and
    // we fall back to the legacy direct dispatch in that case.
    let scored = if let Some(rows) =
        crate::operator_tree_bridge::run_optimised(engine, table, stmt.r#where.as_ref(), params)?
    {
        rows
    } else {
        match stmt.r#where.as_ref() {
            None => engine
                .table_doc_ids(table)
                .into_iter()
                .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
                .collect::<Vec<_>>(),
            Some(Expr::Func { name, args, .. }) if uqa_sql::registry::is_registered(name) => {
                execute_function(engine, table, name, args, params)?
            }
            Some(filter_expr) => execute_mixed_where(engine, table, filter_expr, params)?,
        }
    };

    if has_aggregate(engine, &stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = build_aggregate_rows(engine, table, &scored, stmt, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    if let Some(facet_fields) = facet_projection_fields(&stmt.projections)? {
        return Ok(build_facet_rows(engine, table, &scored, &facet_fields));
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
        let rows = apply_row_order_limit(all_rows, stmt, engine, params)?;
        // Strip the helper fields to keep the projection honest.
        let projected: Vec<_> = rows
            .into_iter()
            .map(|mut row| {
                row.retain(|k, _| columns.iter().any(|c| c == k));
                row
            })
            .collect();
        return Ok(SQLResult::from_rows(columns, projected));
    }

    let scored = apply_order_limit(scored, stmt, engine, params)?;
    let columns = expand_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        engine,
        Some(table),
    );
    let rows = build_rows(engine, table, &scored, &stmt.projections, params)?;
    Ok(SQLResult::from_rows(columns, rows))
}

fn run_single_foreign_select(
    engine: &Engine,
    table: &str,
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let predicates = fdw_predicates_from_where(stmt.r#where.as_ref(), params);
    let scanned = engine
        .scan_foreign_table(table, None, &predicates, None)
        .map_err(SQLError::Unsupported)?;

    let filtered = if let Some(filter) = stmt.r#where.as_ref() {
        let mut out = Vec::with_capacity(scanned.len());
        for row in scanned {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(engine);
            if uqa_sql::expr::eval(filter, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v)) {
                out.push(row);
            }
        }
        out
    } else {
        scanned
    };

    if has_aggregate(engine, &stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &filtered, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    if has_window(&stmt.projections) {
        let columns = projection_columns(&stmt.projections);
        let with_windows = compute_window_columns(engine, &stmt.projections, filtered, params)?;
        let mut rows: Vec<ResultRow> = with_windows
            .iter()
            .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let rows = volcano_project_sort_limit(engine, &filtered, stmt, params)?;
    let columns = expand_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        engine,
        Some(table),
    );
    Ok(SQLResult::from_rows(columns, rows))
}

fn fdw_predicates_from_where(
    expr: Option<&Expr>,
    params: &[SQLParam],
) -> Vec<uqa_fdw::FDWPredicate> {
    let Some(expr) = expr else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_fdw_predicates(expr, params, &mut out);
    out
}

fn collect_fdw_predicates(expr: &Expr, params: &[SQLParam], out: &mut Vec<uqa_fdw::FDWPredicate>) {
    match expr {
        Expr::And(parts) => {
            for part in parts {
                collect_fdw_predicates(part, params, out);
            }
        }
        _ => {
            if let Some(predicate) = fdw_predicate(expr, params) {
                out.push(predicate);
            }
        }
    }
}

fn fdw_predicate(expr: &Expr, params: &[SQLParam]) -> Option<uqa_fdw::FDWPredicate> {
    match expr {
        Expr::Binary { op, lhs, rhs } => {
            if let Some(column) = fdw_column_name(lhs) {
                let value = fdw_const_value(rhs, params)?;
                return Some(uqa_fdw::FDWPredicate {
                    column,
                    operator: fdw_binary_op(*op)?,
                    value,
                });
            }
            if let Some(column) = fdw_column_name(rhs) {
                let value = fdw_const_value(lhs, params)?;
                return Some(uqa_fdw::FDWPredicate {
                    column,
                    operator: fdw_reversed_binary_op(*op)?,
                    value,
                });
            }
            None
        }
        Expr::InList {
            expr,
            list,
            negated,
        } if !negated => {
            let column = fdw_column_name(expr)?;
            let values = list
                .iter()
                .map(|item| fdw_const_value(item, params))
                .collect::<Option<Vec<_>>>()?;
            Some(uqa_fdw::FDWPredicate {
                column,
                operator: uqa_fdw::PredicateOp::In,
                value: Value::List(values),
            })
        }
        Expr::IsNull { expr, negated } => Some(uqa_fdw::FDWPredicate {
            column: fdw_column_name(expr)?,
            operator: if *negated {
                uqa_fdw::PredicateOp::NotEq
            } else {
                uqa_fdw::PredicateOp::Eq
            },
            value: Value::Null,
        }),
        Expr::Func { name, args, .. } => fdw_like_predicate(name, args, false, params),
        Expr::Not(inner) => match inner.as_ref() {
            Expr::Func { name, args, .. } => fdw_like_predicate(name, args, true, params),
            _ => None,
        },
        _ => None,
    }
}

fn fdw_like_predicate(
    name: &str,
    args: &[Expr],
    negated: bool,
    params: &[SQLParam],
) -> Option<uqa_fdw::FDWPredicate> {
    if args.len() != 2 {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let operator = match (lower.as_str(), negated) {
        ("like", false) => uqa_fdw::PredicateOp::Like,
        ("like", true) => uqa_fdw::PredicateOp::NotLike,
        ("ilike", false) => uqa_fdw::PredicateOp::ILike,
        ("ilike", true) => uqa_fdw::PredicateOp::NotILike,
        _ => return None,
    };
    Some(uqa_fdw::FDWPredicate {
        column: fdw_column_name(&args[0])?,
        operator,
        value: fdw_const_value(&args[1], params)?,
    })
}

fn fdw_column_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Column(name) => Some(name.clone()),
        Expr::QualifiedColumn { column, .. } => Some(column.clone()),
        _ => None,
    }
}

fn fdw_const_value(expr: &Expr, params: &[SQLParam]) -> Option<Value> {
    let ctx = uqa_sql::expr::EvalContext::new(None, params);
    uqa_sql::expr::eval(expr, &ctx).ok()
}

fn fdw_binary_op(op: BinaryOp) -> Option<uqa_fdw::PredicateOp> {
    Some(match op {
        BinaryOp::Equal => uqa_fdw::PredicateOp::Eq,
        BinaryOp::NotEqual => uqa_fdw::PredicateOp::NotEq,
        BinaryOp::Less => uqa_fdw::PredicateOp::Lt,
        BinaryOp::LessEqual => uqa_fdw::PredicateOp::LtEq,
        BinaryOp::Greater => uqa_fdw::PredicateOp::Gt,
        BinaryOp::GreaterEqual => uqa_fdw::PredicateOp::GtEq,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => return None,
    })
}

fn fdw_reversed_binary_op(op: BinaryOp) -> Option<uqa_fdw::PredicateOp> {
    Some(match op {
        BinaryOp::Equal => uqa_fdw::PredicateOp::Eq,
        BinaryOp::NotEqual => uqa_fdw::PredicateOp::NotEq,
        BinaryOp::Less => uqa_fdw::PredicateOp::Gt,
        BinaryOp::LessEqual => uqa_fdw::PredicateOp::GtEq,
        BinaryOp::Greater => uqa_fdw::PredicateOp::Lt,
        BinaryOp::GreaterEqual => uqa_fdw::PredicateOp::LtEq,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => return None,
    })
}

fn facet_projection_fields(projections: &[Projection]) -> Result<Option<Vec<String>>, SQLError> {
    if projections.len() != 1 {
        return Ok(None);
    }
    let Expr::Func { name, args, .. } = &projections[0].expr else {
        return Ok(None);
    };
    if !name.eq_ignore_ascii_case("uqa_facets") {
        return Ok(None);
    }
    let mut fields = Vec::with_capacity(args.len());
    for arg in args {
        fields.push(expect_column_name(arg, "uqa_facets.field")?);
    }
    Ok(Some(fields))
}

fn build_facet_rows(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    fields: &[String],
) -> SQLResult {
    let include_field = fields.len() > 1;
    let mut rows = Vec::new();
    for field in fields {
        let mut counts: BTreeMap<Value, i64> = BTreeMap::new();
        for entry in scored {
            let Some(doc) = engine.get_document(table, entry.doc_id) else {
                continue;
            };
            let Some(value) = doc.get(field) else {
                continue;
            };
            if matches!(value, Value::Null) {
                continue;
            }
            *counts.entry(value.clone()).or_insert(0) += 1;
        }
        for (value, count) in counts {
            let mut row = ResultRow::new();
            if include_field {
                row.insert("facet_field".into(), Value::Str(field.clone()));
            }
            row.insert("facet_value".into(), value);
            row.insert("facet_count".into(), Value::Int(count));
            rows.push(row);
        }
    }
    let columns = if include_field {
        vec![
            "facet_field".into(),
            "facet_value".into(),
            "facet_count".into(),
        ]
    } else {
        vec!["facet_value".into(), "facet_count".into()]
    };
    SQLResult {
        columns,
        rows,
        affected_rows: 0,
    }
}

/// When a projection list contains `Expr::Star`, replace the synthetic
/// `*` placeholder in the result column list with the source schema.
/// Empty result sets still report the correct column shape, matching
/// `PostgreSQL`'s behaviour of `SELECT * FROM empty_table`.
pub(super) fn expand_star_columns(
    columns: Vec<String>,
    projections: &[Projection],
    engine: &Engine,
    table: Option<&str>,
) -> Vec<String> {
    let has_star = projections.iter().any(|p| matches!(p.expr, Expr::Star));
    if !has_star {
        return columns;
    }
    let schema_cols: Vec<String> = match table {
        Some(t) => {
            let cols = engine.table_columns(t);
            if cols.is_empty() {
                engine.foreign_table_columns(t)
            } else {
                cols
            }
        }
        None => Vec::new(),
    };
    if schema_cols.is_empty() {
        return columns;
    }
    let mut out: Vec<String> = Vec::with_capacity(columns.len() + schema_cols.len());
    for c in columns {
        if c == "*" {
            for sc in &schema_cols {
                if !out.iter().any(|x| x == sc) {
                    out.push(sc.clone());
                }
            }
        } else if !out.iter().any(|x| x == &c) {
            out.push(c);
        }
    }
    out
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
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut joined = build_join_rows(engine, from, params)?;

    if let Some(filter) = stmt.r#where.as_ref() {
        joined.retain(|row| {
            let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
            uqa_sql::expr::eval(filter, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v))
        });
    }

    if has_aggregate(engine, &stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &joined, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    if has_window(&stmt.projections) {
        let columns = projection_columns(&stmt.projections);
        let with_windows = compute_window_columns(engine, &stmt.projections, joined, params)?;
        let mut rows: Vec<ResultRow> = with_windows
            .iter()
            .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        from,
    );
    let mut rows: Vec<ResultRow> = joined
        .iter()
        .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
        .collect::<Result<_, _>>()?;
    rows = apply_row_order_limit(rows, stmt, engine, params)?;
    Ok(SQLResult::from_rows(columns, rows))
}

pub(super) fn apply_row_order_limit(
    rows: Vec<ResultRow>,
    stmt: &SelectStmt,
    engine: &Engine,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    // Build a Volcano sub-pipeline:  Sort? -> Limit?  on top of an
    // in-memory TableScan over the rows the caller already projected.
    use uqa_execution::physical::{run_to_rows, PhysicalOperator};
    use uqa_execution::relational::{Limit, Sort, SortKey};
    use uqa_execution::scan::TableScan;

    if rows.is_empty() {
        return Ok(rows);
    }
    let resolved_offset = resolve_limit_offset(stmt.offset.as_ref(), engine, params, "OFFSET")?;
    let resolved_limit = resolve_limit_offset(stmt.limit.as_ref(), engine, params, "LIMIT")?;
    if stmt.order_by.is_empty() && resolved_offset.is_none() && resolved_limit.is_none() {
        return Ok(rows);
    }

    let columns: Vec<String> = rows
        .first()
        .map(|r| r.keys().cloned().collect())
        .unwrap_or_default();

    let mut op: Box<dyn PhysicalOperator> = Box::new(TableScan::from_rows(columns, rows));

    if !stmt.order_by.is_empty() {
        let keys: Vec<SortKey> = stmt
            .order_by
            .iter()
            .map(|o| SortKey {
                expr: o.expr.clone(),
                descending: o.descending,
                nulls_first: o
                    .nulls
                    .map(|n| matches!(n, uqa_sql::ast::NullsOrder::First)),
            })
            .collect();
        op = Box::new(Sort::new(op, keys, params.to_vec()));
    }

    if resolved_offset.is_some() || resolved_limit.is_some() {
        op = Box::new(Limit::new(op, resolved_offset.unwrap_or(0), resolved_limit));
    }

    let (_cols, rows) = run_to_rows(op.as_mut()).map_err(|e| match e {
        uqa_execution::physical::ExecError::SQL(err) => err,
        uqa_execution::physical::ExecError::Other(msg) => SQLError::Internal(msg),
    })?;
    Ok(rows)
}

fn explain_int_expr(expr: &Expr) -> String {
    match expr {
        Expr::Literal(Value::Int(n)) => n.to_string(),
        _ => "<expr>".to_string(),
    }
}

/// Evaluate a `LIMIT` / `OFFSET` expression to a non-negative `u64`.
/// Mirrors the canonical UQA implementation's `_extract_int_value` - accepts integer constants,
/// `$N` parameter references, and any expression that the row-evaluator
/// can fold to an integer at execute time. Returns `None` when the
/// clause was absent.
fn resolve_limit_offset(
    expr: Option<&Expr>,
    engine: &Engine,
    params: &[SQLParam],
    label: &str,
) -> Result<Option<u64>, SQLError> {
    let Some(expr) = expr else {
        return Ok(None);
    };
    let ctx = uqa_sql::expr::EvalContext::new(None, params).with_engine(engine);
    let value = uqa_sql::expr::eval(expr, &ctx)?;
    match value {
        Value::Null => Ok(None),
        Value::Int(n) if n >= 0 => Ok(Some(n as u64)),
        Value::Int(_) => Err(SQLError::TypeMismatch(format!(
            "{label} must be non-negative"
        ))),
        Value::Float(f) if f.is_finite() && f >= 0.0 && f.fract() == 0.0 => Ok(Some(f as u64)),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be a non-negative integer, got {other:?}"
        ))),
    }
}

fn apply_order_limit(
    mut entries: Vec<ScoredEntry>,
    stmt: &SelectStmt,
    engine: &Engine,
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if !stmt.order_by.is_empty() {
        // Scored-entry retrieval paths sort by the computed score. Full
        // row projection paths evaluate arbitrary ORDER BY expressions
        // before reaching this helper.
        let descending = stmt.order_by.iter().any(|o| o.descending);
        entries.sort_by(|a, b| {
            let cmp = a
                .score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal);
            if descending { cmp.reverse() } else { cmp }.then_with(|| a.doc_id.cmp(&b.doc_id))
        });
    }
    if let Some(offset) = resolve_limit_offset(stmt.offset.as_ref(), engine, params, "OFFSET")? {
        let off = offset as usize;
        if off >= entries.len() {
            entries.clear();
        } else {
            entries.drain(0..off);
        }
    }
    if let Some(limit) = resolve_limit_offset(stmt.limit.as_ref(), engine, params, "LIMIT")? {
        entries.truncate(limit as usize);
    }
    Ok(entries)
}

pub(super) fn projection_columns(projections: &[Projection]) -> Vec<String> {
    let mut out = Vec::with_capacity(projections.len());
    for proj in projections {
        let base = projection_label_at(proj);
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
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::with_capacity(scored.len());
    for entry in scored {
        let mut document = engine.get_document(table, entry.doc_id).unwrap_or_default();
        document.insert(DOC_ID_COLUMN.into(), Value::Int(entry.doc_id as i64));
        document.insert(SCORE_COLUMN.into(), Value::Float(entry.score));
        let row = build_projection_row(Some(engine), &document, projections, params)?;
        rows.push(row);
    }
    Ok(rows)
}

pub(super) fn build_projection_row(
    engine: Option<&Engine>,
    document: &Document,
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let mut ctx = EvalContext::new(Some(document), params);
    if let Some(e) = engine {
        ctx = ctx.with_engine(e);
    }
    let labels = projection_columns(projections);
    let mut row = ResultRow::new();
    for (idx, proj) in projections.iter().enumerate() {
        let label = labels[idx].clone();
        if let Expr::Star = proj.expr {
            for (k, v) in document {
                if k.as_str() == SCORE_COLUMN
                    || k.as_str() == DOC_ID_COLUMN
                    || k.as_str() == MERGE_ACTION_COLUMN
                {
                    continue;
                }
                row.insert(k.clone(), v.clone());
            }
            continue;
        }
        // Engine-side registry hooks (uqa_highlight / graph_create /
        // graph_drop) need access to the engine; intercept them
        // before falling through to the scalar evaluator.
        if let Expr::Func { name, args, .. } = &proj.expr {
            if let Some(value) = engine_func_intercept(engine, name, args, document, params)? {
                row.insert(label, value);
                continue;
            }
        }
        let value = eval(&proj.expr, &ctx)?;
        row.insert(label, value);
    }
    Ok(row)
}
