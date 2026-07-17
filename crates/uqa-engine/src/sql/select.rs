//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL SELECT, set-operation, CTE, ordering, and projection execution.

use std::cell::RefCell;
use std::collections::HashSet;

use uqa_core::DocId;
use uqa_joins::row_join::JoinKey;

use super::{
    aggregate_join_rows, build_aggregate_rows, build_join_rows_with_ctes,
    build_join_rows_with_ctes_filtered, build_join_rows_with_ctes_filtered_by_qualifier,
    build_join_rows_with_ctes_filtered_filtered_by_qualifier,
    build_join_rows_with_ctes_filtered_pruned,
    build_join_rows_with_ctes_filtered_pruned_filtered_by_qualifier,
    build_join_rows_with_ctes_pruned, build_join_rows_with_ctes_pruned_filtered_by_qualifier,
    compute_window_columns, engine_func_intercept, eval, execute_function,
    execute_function_with_top_k, execute_lateral_subquery, execute_mixed_where, expect_column_name,
    has_aggregate, has_window, project_join_row_with_engine, project_join_row_with_hook,
    project_join_row_with_hook_and_labels, projected_value_from_row, projection_label_at, BTreeMap,
    BTreeSet, BinaryOp, ColumnPrune, Document, Engine, EvalContext, Expr, FromClause, Projection,
    QualifierFilters, ResultRow, SQLError, SQLParam, SQLResult, ScoredEntry, SelectStmt, SetOpKind,
    Statement, Value, CTE, DOC_ID_COLUMN, MERGE_ACTION_COLUMN, SCORE_COLUMN,
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
    let defer_distinct_limit = should_defer_distinct_limit(&stmt);
    let exec_stmt = select_execution_stmt(&stmt, defer_distinct_limit);

    if !exec_stmt.with.is_empty() || exec_stmt.set_op.is_some() {
        let mut ctes = CteScope::new();
        let result = execute_select(engine, &exec_stmt, params, &mut ctes)?;
        return finish_select_result(engine, &stmt, result, params, true, defer_distinct_limit);
    }

    let Some(from) = exec_stmt.from.as_ref() else {
        // SELECT without FROM -- evaluate the projection list against
        // an empty single-row context. Mirrors the canonical UQA implementation's standalone
        // SELECT 1 / SELECT (SELECT ...).
        let result = run_select_without_from(engine, &exec_stmt, params)?;
        return finish_select_result(engine, &stmt, result, params, false, defer_distinct_limit);
    };

    // Single-table FROM with no alias and no window function keeps the
    // search-aware fast path. JOIN shapes and window queries drop into
    // the multi-table executor that builds row tuples up-front and
    // filters them via the expression evaluator.
    if let FromClause::Table { name, alias } = from {
        if alias.is_none() && engine.foreign_table(name).is_some() {
            let result = run_single_foreign_select(engine, name, &exec_stmt, params)?;
            return finish_select_result(
                engine,
                &stmt,
                result,
                params,
                false,
                defer_distinct_limit,
            );
        }
        // Schema-qualified names (information_schema.tables /
        // pg_catalog.pg_*) and CTE references skip the search-aware
        // fast path because they don't correspond to a registered
        // engine table.
        let is_virtual = name.contains('.')
            || (engine.table(name).is_none() && engine.foreign_table(name).is_none());
        let has_subquery_filter = exec_stmt
            .r#where
            .as_ref()
            .is_some_and(expr_contains_subquery);
        if alias.is_none()
            && !has_window(&exec_stmt.projections)
            && !is_virtual
            && !has_subquery_filter
        {
            let result = run_single_table_select(engine, name, &exec_stmt, params)?;
            return finish_select_result(
                engine,
                &stmt,
                result,
                params,
                false,
                defer_distinct_limit,
            );
        }
    }

    let result = run_joined_select(engine, from, &exec_stmt, params)?;
    finish_select_result(engine, &stmt, result, params, false, defer_distinct_limit)
}

fn should_defer_distinct_limit(stmt: &SelectStmt) -> bool {
    stmt.distinct && (stmt.limit.is_some() || stmt.offset.is_some())
}

fn select_execution_stmt(stmt: &SelectStmt, defer_distinct_limit: bool) -> SelectStmt {
    if !defer_distinct_limit {
        return stmt.clone();
    }
    let mut exec_stmt = stmt.clone();
    exec_stmt.limit = None;
    exec_stmt.offset = None;
    exec_stmt
}

fn run_select_without_from(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let row = ResultRow::new();
    let columns = projection_columns(&stmt.projections);
    // `SELECT 1 WHERE false` must produce zero rows: the WHERE clause
    // applies even without a FROM (three-valued: NULL filters too).
    if let Some(filter) = stmt.r#where.as_ref() {
        let ctx = EvalContext::new(Some(&row), params).with_engine(engine);
        let keep = eval(filter, &ctx)?;
        if !uqa_sql::expr::truthy(&keep) {
            return Ok(SQLResult {
                columns,
                rows: Vec::new(),
                affected_rows: 0,
            });
        }
    }
    // Set-returning functions in the projection list expand to rows
    // (`SELECT generate_series(1, 3)`).
    if let Some(result) = expand_projection_srf(engine, stmt, &row, params)? {
        return Ok(result);
    }
    let projected = build_projection_row(Some(engine), &row, &stmt.projections, params)?;
    Ok(SQLResult {
        columns,
        rows: vec![projected],
        affected_rows: 0,
    })
}

/// Expand a projection list that consists of exactly one set-returning
/// function call (`generate_series`, `unnest`, `jsonb_object_keys`,
/// ...) into one result row per element, mirroring `PostgreSQL`'s
/// SRF-in-select-list behavior for the single-SRF case.
fn expand_projection_srf(
    engine: &Engine,
    stmt: &SelectStmt,
    row: &ResultRow,
    params: &[SQLParam],
) -> Result<Option<SQLResult>, SQLError> {
    if stmt.projections.len() != 1 {
        return Ok(None);
    }
    let projection = &stmt.projections[0];
    let Expr::Func { name, args, .. } = &projection.expr else {
        return Ok(None);
    };
    let lower = name.to_ascii_lowercase();
    let columns = projection_columns(&stmt.projections);
    let label = &columns[0];
    // Object-key extractors return a set of rows in PostgreSQL; the
    // scalar evaluator produces the key list, unpacked here.
    if matches!(lower.as_str(), "json_object_keys" | "jsonb_object_keys") {
        let ctx = EvalContext::new(Some(row), params).with_engine(engine);
        let value = eval(&projection.expr, &ctx)?;
        let Value::List(items) = value else {
            return Ok(None);
        };
        let rows: Vec<ResultRow> = items
            .into_iter()
            .map(|item| {
                let mut projected = ResultRow::new();
                projected.insert(label.clone(), item);
                projected
            })
            .collect();
        return Ok(Some(SQLResult {
            columns,
            rows,
            affected_rows: 0,
        }));
    }
    let is_srf = matches!(
        lower.as_str(),
        "generate_series"
            | "unnest"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "regexp_split_to_table"
            | "string_to_table"
    );
    if !is_srf {
        return Ok(None);
    }
    let produced =
        super::from_rows::build_table_function_rows(engine, &lower, args, None, &[], &[], params)?;
    let out: Vec<ResultRow> = produced
        .into_iter()
        .map(|produced_row| {
            let mut projected = ResultRow::new();
            // Table functions emit a single column; relabel it with the
            // projection's alias / function name.
            let value = produced_row
                .iter()
                .next()
                .map_or(Value::Null, |(_, v)| v.clone());
            projected.insert(label.clone(), value);
            projected
        })
        .collect();
    Ok(Some(SQLResult {
        columns,
        rows: out,
        affected_rows: 0,
    }))
}

/// Execute a SELECT that may carry CTEs and/or set ops, returning the
/// final result. CTEs are materialized into the `ctes` map first so the
/// FROM clause can resolve references to them.
pub(super) fn execute_select(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    materialize_ctes(engine, stmt, params, ctes)?;

    // The parent `SelectStmt` carries the LHS branch's own clauses
    // (projections / from / where / group-by / ORDER BY / LIMIT /
    // OFFSET). The set-op-level combined clauses live on
    // `set_op.combined_*`. The LHS branch executes with its own
    // clauses applied; the merged result then takes the combined
    // clauses below.
    let Some(set_op) = stmt.set_op.as_ref() else {
        let lhs = run_query_block(engine, stmt, params, ctes)?;
        let lhs = apply_select_distinct(engine, stmt, lhs, params)?;
        return Ok(lhs);
    };
    let lhs = if let Some(left) = set_op.left.as_deref() {
        execute_select(engine, left, params, ctes)?
    } else {
        let lhs = run_query_block(engine, stmt, params, ctes)?;
        apply_select_distinct(engine, stmt, lhs, params)?
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
            distinct_on: Vec::new(),
        };
        let columns = combined.columns.clone();
        combined.rows = apply_row_order_limit(combined.rows, &synthetic, engine, params)?;
        combined.columns = columns;
    }
    Ok(combined)
}

fn distinct_rows_stable(rows: Vec<ResultRow>) -> Vec<ResultRow> {
    const HASH_THRESHOLD: usize = 64;
    let row_count = rows.len();
    let mut seen_keys: Option<std::collections::HashSet<String>> = None;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(seen) = seen_keys.as_mut() {
            if seen.insert(distinct_row_key(&row)) {
                out.push(row);
            }
            continue;
        }
        if out.iter().any(|existing| existing == &row) {
            continue;
        }
        if out.len() >= HASH_THRESHOLD {
            let mut seen = std::collections::HashSet::with_capacity(row_count);
            for existing in &out {
                seen.insert(distinct_row_key(existing));
            }
            seen.insert(distinct_row_key(&row));
            seen_keys = Some(seen);
            out.push(row);
        } else {
            out.push(row);
        }
    }
    out
}

fn apply_select_distinct(
    engine: &Engine,
    stmt: &SelectStmt,
    mut result: SQLResult,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if stmt.distinct {
        result.rows = if stmt.distinct_on.is_empty() {
            distinct_rows_stable(result.rows)
        } else {
            distinct_on_rows(engine, result.rows, &stmt.distinct_on, params)?
        };
    }
    Ok(result)
}

fn finish_select_result(
    engine: &Engine,
    stmt: &SelectStmt,
    mut result: SQLResult,
    params: &[SQLParam],
    distinct_already_applied: bool,
    apply_deferred_limit: bool,
) -> Result<SQLResult, SQLError> {
    if !distinct_already_applied {
        result = apply_select_distinct(engine, stmt, result, params)?;
    }
    if apply_deferred_limit {
        let columns = result.columns.clone();
        result.rows = apply_limit_offset_only(result.rows, stmt, engine, params)?;
        result.columns = columns;
    }
    Ok(result)
}

fn apply_limit_offset_only(
    rows: Vec<ResultRow>,
    stmt: &SelectStmt,
    engine: &Engine,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let synthetic = SelectStmt {
        projections: Vec::new(),
        from: None,
        r#where: None,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        having: None,
        order_by: Vec::new(),
        limit: stmt.limit.clone(),
        offset: stmt.offset.clone(),
        with: Vec::new(),
        set_op: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    apply_row_order_limit(rows, &synthetic, engine, params)
}

fn distinct_on_rows(
    engine: &Engine,
    rows: Vec<ResultRow>,
    keys: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(rows.len());
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(engine);
        let mut key = String::new();
        for expr in keys {
            let value = eval(expr, &ctx)?;
            push_distinct_key_segment(&mut key, &distinct_value_key(&value));
        }
        if seen.insert(key) {
            out.push(row);
        }
    }
    Ok(out)
}

fn distinct_row_key(row: &ResultRow) -> String {
    let mut key = String::new();
    for (column, value) in row {
        push_distinct_key_segment(&mut key, column);
        push_distinct_key_segment(&mut key, &distinct_value_key(value));
    }
    key
}

fn push_distinct_key_segment(key: &mut String, segment: &str) {
    key.push_str(&segment.len().to_string());
    key.push(':');
    key.push_str(segment);
}

fn distinct_value_key(value: &Value) -> String {
    match value {
        Value::Null => "\x00".into(),
        Value::Bool(value) => format!("b:{value}"),
        Value::Int(value) => format!("i:{value}"),
        Value::Float(value) => format!("f:{value:.17}"),
        Value::Str(value) => format!("s:{value}"),
        Value::Bytes(value) => format!("y:{value:?}"),
        Value::Temporal(value) => format!("t:{}", value.to_sql_string()),
        other => format!("o:{other:?}"),
    }
}

type SubqueryCache = BTreeMap<String, (Vec<String>, Vec<ResultRow>)>;

#[derive(Clone, Default)]
pub(crate) struct CteScope {
    pub(super) rows: BTreeMap<String, Vec<ResultRow>>,
    pub(super) inlined: BTreeMap<String, SelectStmt>,
}

impl CteScope {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(super) fn insert_materialized(&mut self, name: String, rows: Vec<ResultRow>) {
        self.remove_prefixed_row_caches(&name);
        self.rows.insert(name, rows);
    }

    pub(super) fn remove_materialized(&mut self, name: &str) -> Option<Vec<ResultRow>> {
        self.remove_prefixed_row_caches(name);
        self.rows.remove(name)
    }

    fn remove_prefixed_row_caches(&mut self, name: &str) {
        let prefix = format!("__uqa_internal_prefixed_cte_cache__:{name}:");
        self.rows.retain(|key, _| !key.starts_with(&prefix));
    }
}

pub(super) struct ScopedEngineHook<'a> {
    engine: &'a Engine,
    ctes: &'a CteScope,
    subquery_cache: RefCell<SubqueryCache>,
}

impl<'a> ScopedEngineHook<'a> {
    pub(super) fn new(engine: &'a Engine, ctes: &'a CteScope) -> Self {
        Self {
            engine,
            ctes,
            subquery_cache: RefCell::new(BTreeMap::new()),
        }
    }
}

struct ExistsMembershipPlan {
    filters: Vec<ExistsMembershipFilter>,
    residual: Option<Expr>,
}

struct ExistsMembershipFilter {
    outer_exprs: Vec<Expr>,
    inner_keys: HashSet<Vec<JoinKey>>,
    negated: bool,
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

    fn has_scalar_functions(&self) -> bool {
        self.engine.has_registered_scalar_functions()
    }

    fn call_user_function(
        &self,
        name: &str,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        crate::sql::call_user_scalar_function(self.engine, name, args)
    }

    fn run_subquery(
        &self,
        stmt: &uqa_sql::ast::SelectStmt,
        outer_row: Option<&ResultRow>,
        params: &[SQLParam],
    ) -> std::result::Result<(Vec<String>, Vec<ResultRow>), String> {
        let correlated = outer_row.is_some() && select_references_outer(stmt);
        let cache_key = if select_contains_volatile_function(stmt) {
            None
        } else if correlated {
            outer_row.and_then(|row| correlated_subquery_cache_key(stmt, row))
        } else {
            Some(format!("uncorrelated:{stmt:?}"))
        };
        if let Some(key) = cache_key {
            if let Some(cached) = self.subquery_cache.borrow().get(&key) {
                return Ok(cached.clone());
            }
            let result = run_correlated_subquery(
                self.engine,
                stmt,
                if correlated { outer_row } else { None },
                params,
                self.ctes,
            )
            .map(|result| (result.columns, result.rows))
            .map_err(|e| format!("subquery failed: {e}"))?;
            self.subquery_cache.borrow_mut().insert(key, result.clone());
            return Ok(result);
        }
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
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    if let Some(row) = outer_row {
        execute_lateral_subquery(engine, stmt, row, params, ctes)
    } else {
        let mut scoped_ctes = ctes.clone();
        execute_select(engine, stmt, params, &mut scoped_ctes)
    }
}

fn select_references_outer(stmt: &SelectStmt) -> bool {
    let mut local_qualifiers = std::collections::BTreeSet::new();
    if let Some(from) = stmt.from.as_ref() {
        collect_local_qualifiers(from, &mut local_qualifiers);
    }
    let has_local_from = stmt.from.is_some();
    stmt.projections.iter().any(|projection| {
        expr_references_outer(&projection.expr, &local_qualifiers, has_local_from)
    }) || stmt
        .r#where
        .as_ref()
        .is_some_and(|expr| expr_references_outer(expr, &local_qualifiers, has_local_from))
        || stmt
            .group_by
            .iter()
            .any(|expr| expr_references_outer(expr, &local_qualifiers, has_local_from))
        || stmt.grouping_sets.iter().any(|set| {
            set.iter()
                .any(|expr| expr_references_outer(expr, &local_qualifiers, has_local_from))
        })
        || stmt
            .having
            .as_ref()
            .is_some_and(|expr| expr_references_outer(expr, &local_qualifiers, has_local_from))
        || stmt
            .order_by
            .iter()
            .any(|order| expr_references_outer(&order.expr, &local_qualifiers, has_local_from))
        || stmt
            .limit
            .as_ref()
            .is_some_and(|expr| expr_references_outer(expr, &local_qualifiers, has_local_from))
        || stmt
            .offset
            .as_ref()
            .is_some_and(|expr| expr_references_outer(expr, &local_qualifiers, has_local_from))
        || stmt
            .distinct_on
            .iter()
            .any(|expr| expr_references_outer(expr, &local_qualifiers, has_local_from))
        || stmt.set_op.as_ref().is_some_and(|set_op| {
            set_op.left.as_deref().is_some_and(select_references_outer)
                || select_references_outer(&set_op.right)
        })
}

fn correlated_subquery_cache_key(stmt: &SelectStmt, outer_row: &ResultRow) -> Option<String> {
    let refs = select_outer_references(stmt);
    if refs.is_empty() {
        return None;
    }
    let mut key = format!("correlated:{stmt:?}:");
    for (qualifier, column) in refs {
        push_distinct_key_segment(&mut key, &qualifier);
        push_distinct_key_segment(&mut key, &column);
        let value = outer_reference_value(outer_row, &qualifier, &column);
        push_distinct_key_segment(&mut key, &distinct_value_key(&value));
    }
    Some(key)
}

fn select_outer_references(stmt: &SelectStmt) -> Vec<(String, String)> {
    let mut local_qualifiers = std::collections::BTreeSet::new();
    if let Some(from) = stmt.from.as_ref() {
        collect_local_qualifiers(from, &mut local_qualifiers);
    }
    let has_local_from = stmt.from.is_some();
    let mut refs = std::collections::BTreeSet::new();
    for projection in &stmt.projections {
        collect_expr_outer_references(
            &projection.expr,
            &local_qualifiers,
            has_local_from,
            &mut refs,
        );
    }
    if let Some(expr) = stmt.r#where.as_ref() {
        collect_expr_outer_references(expr, &local_qualifiers, has_local_from, &mut refs);
    }
    for expr in &stmt.group_by {
        collect_expr_outer_references(expr, &local_qualifiers, has_local_from, &mut refs);
    }
    for set in &stmt.grouping_sets {
        for expr in set {
            collect_expr_outer_references(expr, &local_qualifiers, has_local_from, &mut refs);
        }
    }
    if let Some(expr) = stmt.having.as_ref() {
        collect_expr_outer_references(expr, &local_qualifiers, has_local_from, &mut refs);
    }
    for order in &stmt.order_by {
        collect_expr_outer_references(&order.expr, &local_qualifiers, has_local_from, &mut refs);
    }
    if let Some(expr) = stmt.limit.as_ref() {
        collect_expr_outer_references(expr, &local_qualifiers, has_local_from, &mut refs);
    }
    if let Some(expr) = stmt.offset.as_ref() {
        collect_expr_outer_references(expr, &local_qualifiers, has_local_from, &mut refs);
    }
    for expr in &stmt.distinct_on {
        collect_expr_outer_references(expr, &local_qualifiers, has_local_from, &mut refs);
    }
    if let Some(set_op) = stmt.set_op.as_ref() {
        if let Some(left) = set_op.left.as_deref() {
            refs.extend(select_outer_references(left));
        }
        refs.extend(select_outer_references(&set_op.right));
    }
    refs.into_iter().collect()
}

fn outer_reference_value(row: &ResultRow, qualifier: &str, column: &str) -> Value {
    if qualifier.is_empty() {
        return row.get(column).cloned().unwrap_or(Value::Null);
    }
    let lookup_key = format!("{qualifier}.{column}");
    row.get(&lookup_key)
        .or_else(|| row.get(column))
        .cloned()
        .unwrap_or(Value::Null)
}

fn collect_local_qualifiers(from: &FromClause, out: &mut std::collections::BTreeSet<String>) {
    match from {
        FromClause::Table { name, alias } => {
            out.insert(name.clone());
            if let Some(alias) = alias {
                out.insert(alias.clone());
            }
        }
        FromClause::Join { left, right, .. } => {
            collect_local_qualifiers(left, out);
            collect_local_qualifiers(right, out);
        }
        FromClause::Values { alias, .. }
        | FromClause::Function { alias, .. }
        | FromClause::Subquery { alias, .. } => {
            if let Some(alias) = alias {
                out.insert(alias.clone());
            }
        }
    }
}

fn collect_expr_outer_references(
    expr: &Expr,
    local_qualifiers: &std::collections::BTreeSet<String>,
    has_local_from: bool,
    refs: &mut std::collections::BTreeSet<(String, String)>,
) {
    match expr {
        Expr::Column(column) => {
            if !has_local_from {
                refs.insert((String::new(), column.clone()));
            }
        }
        Expr::QualifiedColumn {
            qualifier, column, ..
        } => {
            if !local_qualifiers.contains(qualifier) {
                refs.insert((qualifier.clone(), column.clone()));
            }
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_expr_outer_references(item, local_qualifiers, has_local_from, refs);
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for arg in args {
                collect_expr_outer_references(arg, local_qualifiers, has_local_from, refs);
            }
            for order in order_by {
                collect_expr_outer_references(&order.expr, local_qualifiers, has_local_from, refs);
            }
            if let Some(filter) = filter.as_ref() {
                collect_expr_outer_references(filter, local_qualifiers, has_local_from, refs);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_outer_references(lhs, local_qualifiers, has_local_from, refs);
            collect_expr_outer_references(rhs, local_qualifiers, has_local_from, refs);
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            collect_expr_outer_references(inner, local_qualifiers, has_local_from, refs);
        }
        Expr::Between { expr, low, high } => {
            collect_expr_outer_references(expr, local_qualifiers, has_local_from, refs);
            collect_expr_outer_references(low, local_qualifiers, has_local_from, refs);
            collect_expr_outer_references(high, local_qualifiers, has_local_from, refs);
        }
        Expr::InList { expr, list, .. } => {
            collect_expr_outer_references(expr, local_qualifiers, has_local_from, refs);
            for item in list {
                collect_expr_outer_references(item, local_qualifiers, has_local_from, refs);
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for arg in args {
                collect_expr_outer_references(arg, local_qualifiers, has_local_from, refs);
            }
            for arg in &spec.partition_by {
                collect_expr_outer_references(arg, local_qualifiers, has_local_from, refs);
            }
            for order in &spec.order_by {
                collect_expr_outer_references(&order.expr, local_qualifiers, has_local_from, refs);
            }
            if let Some(frame) = spec.frame.as_ref() {
                collect_frame_bound_outer_references(
                    &frame.start,
                    local_qualifiers,
                    has_local_from,
                    refs,
                );
                collect_frame_bound_outer_references(
                    &frame.end,
                    local_qualifiers,
                    has_local_from,
                    refs,
                );
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_ref() {
                collect_expr_outer_references(base, local_qualifiers, has_local_from, refs);
            }
            for (cond, result) in when {
                collect_expr_outer_references(cond, local_qualifiers, has_local_from, refs);
                collect_expr_outer_references(result, local_qualifiers, has_local_from, refs);
            }
            if let Some(else_branch) = else_branch.as_ref() {
                collect_expr_outer_references(else_branch, local_qualifiers, has_local_from, refs);
            }
        }
        Expr::Star
        | Expr::Literal(_)
        | Expr::Param(_)
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => {}
    }
}

fn collect_frame_bound_outer_references(
    bound: &uqa_sql::ast::FrameBound,
    local_qualifiers: &std::collections::BTreeSet<String>,
    has_local_from: bool,
    refs: &mut std::collections::BTreeSet<(String, String)>,
) {
    match bound {
        uqa_sql::ast::FrameBound::Preceding(expr) | uqa_sql::ast::FrameBound::Following(expr) => {
            collect_expr_outer_references(expr, local_qualifiers, has_local_from, refs);
        }
        uqa_sql::ast::FrameBound::UnboundedPreceding
        | uqa_sql::ast::FrameBound::UnboundedFollowing
        | uqa_sql::ast::FrameBound::CurrentRow => {}
    }
}

fn prepare_exists_membership_filter(
    engine: &Engine,
    filter: &Expr,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<Option<ExistsMembershipPlan>, SQLError> {
    match filter {
        _ if exists_predicate_parts(filter).is_some() => {
            let Some(filter) = prepare_single_exists_membership(engine, filter, params, ctes)?
            else {
                return Ok(None);
            };
            Ok(Some(ExistsMembershipPlan {
                filters: vec![filter],
                residual: None,
            }))
        }
        Expr::And(items) => {
            let mut filters = Vec::new();
            let mut residual = Vec::new();
            for item in items {
                if exists_predicate_parts(item).is_some() {
                    let Some(filter) =
                        prepare_single_exists_membership(engine, item, params, ctes)?
                    else {
                        return Ok(None);
                    };
                    filters.push(filter);
                } else if !expr_contains_subquery(item) && !expr_contains_volatile_function(item) {
                    residual.push(item.clone());
                } else {
                    return Ok(None);
                }
            }
            if filters.is_empty() {
                return Ok(None);
            }
            Ok(Some(ExistsMembershipPlan {
                filters,
                residual: combine_and_items(residual),
            }))
        }
        _ => Ok(None),
    }
}

fn exists_membership_filter_is_stable(engine: &Engine, filter: &Expr, ctes: &CteScope) -> bool {
    match filter {
        _ if exists_predicate_parts(filter).is_some() => exists_predicate_parts(filter)
            .and_then(|(body, _)| body.from.as_ref())
            .is_some_and(|from| exists_membership_from_is_stable(engine, from, ctes)),
        Expr::And(items) => items.iter().all(|item| {
            if exists_predicate_parts(item).is_some() {
                exists_membership_filter_is_stable(engine, item, ctes)
            } else {
                !expr_contains_subquery(item) && !expr_contains_volatile_function(item)
            }
        }),
        _ => false,
    }
}

fn prepare_single_exists_membership(
    engine: &Engine,
    filter: &Expr,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<Option<ExistsMembershipFilter>, SQLError> {
    let Some((body, negated)) = exists_predicate_parts(filter) else {
        return Ok(None);
    };
    if !body.with.is_empty()
        || body.set_op.is_some()
        || body.distinct
        || !body.distinct_on.is_empty()
        || !body.group_by.is_empty()
        || !body.grouping_sets.is_empty()
        || body.having.is_some()
        || !body.order_by.is_empty()
        || body.limit.is_some()
        || body.offset.is_some()
        || select_contains_volatile_function(body)
    {
        return Ok(None);
    }
    let Some(from) = body.from.as_ref() else {
        return Ok(None);
    };
    let Some(where_expr) = body.r#where.as_ref() else {
        return Ok(None);
    };

    let mut local_qualifiers = std::collections::BTreeSet::new();
    collect_local_qualifiers(from, &mut local_qualifiers);
    let Some((inner_exprs, outer_exprs, local_filter)) =
        split_exists_membership_where(where_expr, &local_qualifiers)
    else {
        return Ok(None);
    };

    let inner_rows = build_join_rows_with_ctes(engine, from, params, ctes)?;
    let mut inner_keys = HashSet::with_capacity(inner_rows.len());
    for row in &inner_rows {
        if let Some(local_filter) = local_filter.as_ref() {
            let ctx = EvalContext::new(Some(row), params).with_engine(engine);
            if !eval(local_filter, &ctx).is_ok_and(|value| uqa_sql::expr::truthy(&value)) {
                continue;
            }
        }
        let ctx = EvalContext::new(Some(row), params).with_engine(engine);
        if let Some(key) = membership_key_for_exprs(&inner_exprs, &ctx)? {
            inner_keys.insert(key);
        }
    }

    Ok(Some(ExistsMembershipFilter {
        outer_exprs,
        inner_keys,
        negated,
    }))
}

fn exists_membership_from_is_stable(engine: &Engine, from: &FromClause, ctes: &CteScope) -> bool {
    let mut tables = Vec::new();
    from.collect_tables(&mut tables);
    !tables.is_empty()
        && tables.iter().all(|(name, _)| {
            engine.table(name).is_some()
                && !ctes.rows.contains_key(name)
                && !ctes.inlined.contains_key(name)
        })
}

fn exists_predicate_parts(expr: &Expr) -> Option<(&SelectStmt, bool)> {
    match expr {
        Expr::Exists { body, negated } => Some((body, *negated)),
        Expr::Not(inner) => match inner.as_ref() {
            Expr::Exists { body, negated } => Some((body, !*negated)),
            _ => None,
        },
        _ => None,
    }
}

fn split_exists_membership_where(
    expr: &Expr,
    local_qualifiers: &std::collections::BTreeSet<String>,
) -> Option<(Vec<Expr>, Vec<Expr>, Option<Expr>)> {
    match expr {
        Expr::And(items) => {
            let mut inner_exprs = Vec::new();
            let mut outer_exprs = Vec::new();
            let mut local_filters = Vec::new();
            for item in items {
                if let Some((inner_expr, outer_expr)) =
                    split_correlated_equality(item, local_qualifiers)
                {
                    inner_exprs.push(inner_expr);
                    outer_exprs.push(outer_expr);
                } else if !expr_references_outer(item, local_qualifiers, true)
                    && !expr_contains_subquery(item)
                    && !expr_contains_volatile_function(item)
                {
                    local_filters.push(item.clone());
                } else {
                    return None;
                }
            }
            if inner_exprs.is_empty() {
                return None;
            }
            Some((inner_exprs, outer_exprs, combine_and_items(local_filters)))
        }
        other => split_correlated_equality(other, local_qualifiers)
            .map(|(inner_expr, outer_expr)| (vec![inner_expr], vec![outer_expr], None)),
    }
}

fn combine_and_items(items: Vec<Expr>) -> Option<Expr> {
    match items.len() {
        0 => None,
        1 => items.into_iter().next(),
        _ => Some(Expr::And(items)),
    }
}

fn split_correlated_equality(
    expr: &Expr,
    local_qualifiers: &std::collections::BTreeSet<String>,
) -> Option<(Expr, Expr)> {
    let Expr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    } = expr
    else {
        return None;
    };
    match (
        classify_exists_expr_side(lhs, local_qualifiers)?,
        classify_exists_expr_side(rhs, local_qualifiers)?,
    ) {
        (ExistsExprSide::Inner, ExistsExprSide::Outer) => Some(((**lhs).clone(), (**rhs).clone())),
        (ExistsExprSide::Outer, ExistsExprSide::Inner) => Some(((**rhs).clone(), (**lhs).clone())),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExistsExprSide {
    Inner,
    Outer,
}

fn classify_exists_expr_side(
    expr: &Expr,
    local_qualifiers: &std::collections::BTreeSet<String>,
) -> Option<ExistsExprSide> {
    if expr_contains_subquery(expr) || expr_contains_volatile_function(expr) {
        return None;
    }
    if expr_references_outer(expr, local_qualifiers, true) {
        Some(ExistsExprSide::Outer)
    } else {
        Some(ExistsExprSide::Inner)
    }
}

fn apply_exists_membership_filter(
    engine: &Engine,
    rows: Vec<ResultRow>,
    plan: &ExistsMembershipPlan,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let ctx = EvalContext::new(Some(&row), params).with_engine(engine);
        if let Some(residual) = plan.residual.as_ref() {
            if !eval(residual, &ctx).is_ok_and(|value| uqa_sql::expr::truthy(&value)) {
                continue;
            }
        }
        let mut keep = true;
        for filter in &plan.filters {
            let contains = membership_key_for_exprs(&filter.outer_exprs, &ctx)?
                .is_some_and(|key| filter.inner_keys.contains(&key));
            if contains == filter.negated {
                keep = false;
                break;
            }
        }
        if keep {
            out.push(row);
        }
    }
    Ok(out)
}

fn exists_membership_plan_applicable_to_rows(
    plan: &ExistsMembershipPlan,
    rows: &[ResultRow],
) -> bool {
    rows.first()
        .is_some_and(|row| exists_membership_plan_applicable_to_row(plan, row))
}

fn exists_membership_plan_applicable_to_row(plan: &ExistsMembershipPlan, row: &ResultRow) -> bool {
    plan.residual
        .as_ref()
        .is_none_or(|expr| expr_applicable_to_row(expr, row))
        && plan.filters.iter().all(|filter| {
            filter
                .outer_exprs
                .iter()
                .all(|expr| expr_applicable_to_row(expr, row))
        })
}

fn expr_applicable_to_row(expr: &Expr, row: &ResultRow) -> bool {
    match expr {
        Expr::Column(name) => column_present(row, name),
        Expr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => {
            if key.is_empty() {
                row.contains_key(&format!("{qualifier}.{column}"))
            } else {
                row.contains_key(key)
            }
        }
        Expr::Literal(_) | Expr::Param(_) => true,
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().all(|expr| expr_applicable_to_row(expr, row))
                && order_by
                    .iter()
                    .all(|order| expr_applicable_to_row(&order.expr, row))
                && filter
                    .as_ref()
                    .is_none_or(|expr| expr_applicable_to_row(expr, row))
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().all(|expr| expr_applicable_to_row(expr, row))
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_applicable_to_row(lhs, row) && expr_applicable_to_row(rhs, row)
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expr_applicable_to_row(inner, row)
        }
        Expr::Between { expr, low, high } => {
            expr_applicable_to_row(expr, row)
                && expr_applicable_to_row(low, row)
                && expr_applicable_to_row(high, row)
        }
        Expr::InList { expr, list, .. } => {
            expr_applicable_to_row(expr, row)
                && list.iter().all(|item| expr_applicable_to_row(item, row))
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_none_or(|expr| expr_applicable_to_row(expr, row))
                && when.iter().all(|(condition, result)| {
                    expr_applicable_to_row(condition, row) && expr_applicable_to_row(result, row)
                })
                && else_branch
                    .as_ref()
                    .is_none_or(|expr| expr_applicable_to_row(expr, row))
        }
        Expr::Star
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => false,
    }
}

fn column_present(row: &ResultRow, name: &str) -> bool {
    row.contains_key(name)
        || row.keys().any(|key| {
            key.rsplit_once('.')
                .is_some_and(|(_, column)| column == name)
        })
}

fn membership_key_for_exprs(
    exprs: &[Expr],
    ctx: &EvalContext<'_>,
) -> Result<Option<Vec<JoinKey>>, SQLError> {
    let mut key = Vec::with_capacity(exprs.len());
    for expr in exprs {
        let value = eval(expr, ctx)?;
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        key.push(JoinKey::new(&value));
    }
    Ok(Some(key))
}

fn precompute_uncorrelated_subqueries(
    engine: &Engine,
    expr: &Expr,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Expr, SQLError> {
    match expr {
        Expr::ScalarSubquery(body)
            if !select_references_outer(body) && !select_contains_volatile_function(body) =>
        {
            Ok(Expr::Literal(evaluate_scalar_subquery_once(
                engine, body, params, ctes,
            )?))
        }
        Expr::Exists { body, negated }
            if !select_references_outer(body) && !select_contains_volatile_function(body) =>
        {
            let result = run_correlated_subquery(engine, body, None, params, ctes)?;
            let exists = !result.rows.is_empty();
            Ok(Expr::Literal(Value::Bool(if *negated {
                !exists
            } else {
                exists
            })))
        }
        Expr::Array(items) => Ok(Expr::Array(
            items
                .iter()
                .map(|item| precompute_uncorrelated_subqueries(engine, item, params, ctes))
                .collect::<Result<_, _>>()?,
        )),
        Expr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => Ok(Expr::Func {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| precompute_uncorrelated_subqueries(engine, arg, params, ctes))
                .collect::<Result<_, _>>()?,
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_ref()
                .map(|filter| precompute_uncorrelated_subqueries(engine, filter, params, ctes))
                .transpose()?
                .map(Box::new),
        }),
        Expr::Binary { op, lhs, rhs } => Ok(Expr::Binary {
            op: *op,
            lhs: Box::new(precompute_uncorrelated_subqueries(
                engine, lhs, params, ctes,
            )?),
            rhs: Box::new(precompute_uncorrelated_subqueries(
                engine, rhs, params, ctes,
            )?),
        }),
        Expr::Not(inner) => Ok(Expr::Not(Box::new(precompute_uncorrelated_subqueries(
            engine, inner, params, ctes,
        )?))),
        Expr::And(items) => Ok(Expr::And(
            items
                .iter()
                .map(|item| precompute_uncorrelated_subqueries(engine, item, params, ctes))
                .collect::<Result<_, _>>()?,
        )),
        Expr::Or(items) => Ok(Expr::Or(
            items
                .iter()
                .map(|item| precompute_uncorrelated_subqueries(engine, item, params, ctes))
                .collect::<Result<_, _>>()?,
        )),
        Expr::IsNull { expr, negated } => Ok(Expr::IsNull {
            expr: Box::new(precompute_uncorrelated_subqueries(
                engine, expr, params, ctes,
            )?),
            negated: *negated,
        }),
        Expr::Between { expr, low, high } => Ok(Expr::Between {
            expr: Box::new(precompute_uncorrelated_subqueries(
                engine, expr, params, ctes,
            )?),
            low: Box::new(precompute_uncorrelated_subqueries(
                engine, low, params, ctes,
            )?),
            high: Box::new(precompute_uncorrelated_subqueries(
                engine, high, params, ctes,
            )?),
        }),
        Expr::InList {
            expr,
            list,
            negated,
        } => Ok(Expr::InList {
            expr: Box::new(precompute_uncorrelated_subqueries(
                engine, expr, params, ctes,
            )?),
            list: list
                .iter()
                .map(|item| precompute_uncorrelated_subqueries(engine, item, params, ctes))
                .collect::<Result<_, _>>()?,
            negated: *negated,
        }),
        Expr::WindowCall { name, args, spec } => Ok(Expr::WindowCall {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| precompute_uncorrelated_subqueries(engine, arg, params, ctes))
                .collect::<Result<_, _>>()?,
            spec: spec.clone(),
        }),
        Expr::Case {
            base,
            when,
            else_branch,
        } => Ok(Expr::Case {
            base: base
                .as_ref()
                .map(|expr| precompute_uncorrelated_subqueries(engine, expr, params, ctes))
                .transpose()?
                .map(Box::new),
            when: when
                .iter()
                .map(|(cond, result)| {
                    Ok((
                        precompute_uncorrelated_subqueries(engine, cond, params, ctes)?,
                        precompute_uncorrelated_subqueries(engine, result, params, ctes)?,
                    ))
                })
                .collect::<Result<_, SQLError>>()?,
            else_branch: else_branch
                .as_ref()
                .map(|expr| precompute_uncorrelated_subqueries(engine, expr, params, ctes))
                .transpose()?
                .map(Box::new),
        }),
        Expr::Cast { expr, ty } => Ok(Expr::Cast {
            expr: Box::new(precompute_uncorrelated_subqueries(
                engine, expr, params, ctes,
            )?),
            ty: ty.clone(),
        }),
        _ => Ok(expr.clone()),
    }
}

fn evaluate_scalar_subquery_once(
    engine: &Engine,
    body: &SelectStmt,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Value, SQLError> {
    let result = run_correlated_subquery(engine, body, None, params, ctes)?;
    if result.rows.is_empty() {
        return Ok(Value::Null);
    }
    if result.rows.len() > 1 {
        return Err(SQLError::TypeMismatch(
            "scalar subquery returned more than one row".into(),
        ));
    }
    let first_col = result
        .columns
        .first()
        .ok_or_else(|| SQLError::TypeMismatch("scalar subquery returned no columns".into()))?;
    Ok(result.rows[0]
        .get(first_col)
        .cloned()
        .unwrap_or(Value::Null))
}

fn expr_references_outer(
    expr: &Expr,
    local_qualifiers: &std::collections::BTreeSet<String>,
    has_local_from: bool,
) -> bool {
    match expr {
        Expr::Star | Expr::Literal(_) | Expr::Param(_) => false,
        Expr::Column(_) => !has_local_from,
        Expr::QualifiedColumn { qualifier, .. } => !local_qualifiers.contains(qualifier),
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => items
            .iter()
            .any(|item| expr_references_outer(item, local_qualifiers, has_local_from)),
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter()
                .any(|arg| expr_references_outer(arg, local_qualifiers, has_local_from))
                || order_by.iter().any(|order| {
                    expr_references_outer(&order.expr, local_qualifiers, has_local_from)
                })
                || filter
                    .as_ref()
                    .is_some_and(|arg| expr_references_outer(arg, local_qualifiers, has_local_from))
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_references_outer(lhs, local_qualifiers, has_local_from)
                || expr_references_outer(rhs, local_qualifiers, has_local_from)
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expr_references_outer(inner, local_qualifiers, has_local_from)
        }
        Expr::Between { expr, low, high } => {
            expr_references_outer(expr, local_qualifiers, has_local_from)
                || expr_references_outer(low, local_qualifiers, has_local_from)
                || expr_references_outer(high, local_qualifiers, has_local_from)
        }
        Expr::InList { expr, list, .. } => {
            expr_references_outer(expr, local_qualifiers, has_local_from)
                || list
                    .iter()
                    .any(|item| expr_references_outer(item, local_qualifiers, has_local_from))
        }
        Expr::WindowCall { args, spec, .. } => {
            args.iter()
                .any(|arg| expr_references_outer(arg, local_qualifiers, has_local_from))
                || spec
                    .partition_by
                    .iter()
                    .any(|arg| expr_references_outer(arg, local_qualifiers, has_local_from))
                || spec.order_by.iter().any(|order| {
                    expr_references_outer(&order.expr, local_qualifiers, has_local_from)
                })
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_references_outer(&frame.start, local_qualifiers, has_local_from)
                        || frame_bound_references_outer(
                            &frame.end,
                            local_qualifiers,
                            has_local_from,
                        )
                })
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| expr_references_outer(expr, local_qualifiers, has_local_from))
                || when.iter().any(|(cond, result)| {
                    expr_references_outer(cond, local_qualifiers, has_local_from)
                        || expr_references_outer(result, local_qualifiers, has_local_from)
                })
                || else_branch.as_ref().is_some_and(|expr| {
                    expr_references_outer(expr, local_qualifiers, has_local_from)
                })
        }
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => true,
    }
}

fn expr_contains_subquery(expr: &Expr) -> bool {
    match expr {
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => true,
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().any(expr_contains_subquery)
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_contains_subquery)
                || order_by
                    .iter()
                    .any(|order| expr_contains_subquery(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|expr| expr_contains_subquery(expr))
        }
        Expr::Binary { lhs, rhs, .. } => expr_contains_subquery(lhs) || expr_contains_subquery(rhs),
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expr_contains_subquery(inner)
        }
        Expr::Between { expr, low, high } => {
            expr_contains_subquery(expr)
                || expr_contains_subquery(low)
                || expr_contains_subquery(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_contains_subquery(expr) || list.iter().any(expr_contains_subquery)
        }
        Expr::WindowCall { args, spec, .. } => {
            args.iter().any(expr_contains_subquery)
                || spec.partition_by.iter().any(expr_contains_subquery)
                || spec
                    .order_by
                    .iter()
                    .any(|order| expr_contains_subquery(&order.expr))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_contains_subquery(&frame.start)
                        || frame_bound_contains_subquery(&frame.end)
                })
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| expr_contains_subquery(expr))
                || when.iter().any(|(cond, result)| {
                    expr_contains_subquery(cond) || expr_contains_subquery(result)
                })
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_contains_subquery(expr))
        }
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => false,
    }
}

pub(super) fn select_contains_volatile_function(stmt: &SelectStmt) -> bool {
    stmt.projections
        .iter()
        .any(|projection| expr_contains_volatile_function(&projection.expr))
        || stmt
            .r#where
            .as_ref()
            .is_some_and(expr_contains_volatile_function)
        || stmt.group_by.iter().any(expr_contains_volatile_function)
        || stmt
            .grouping_sets
            .iter()
            .any(|set| set.iter().any(expr_contains_volatile_function))
        || stmt
            .having
            .as_ref()
            .is_some_and(expr_contains_volatile_function)
        || stmt
            .order_by
            .iter()
            .any(|order| expr_contains_volatile_function(&order.expr))
        || stmt
            .limit
            .as_ref()
            .is_some_and(expr_contains_volatile_function)
        || stmt
            .offset
            .as_ref()
            .is_some_and(expr_contains_volatile_function)
        || stmt.distinct_on.iter().any(expr_contains_volatile_function)
        || stmt.set_op.as_ref().is_some_and(|set_op| {
            set_op
                .left
                .as_deref()
                .is_some_and(select_contains_volatile_function)
                || select_contains_volatile_function(&set_op.right)
                || set_op
                    .combined_order_by
                    .iter()
                    .any(|order| expr_contains_volatile_function(&order.expr))
                || set_op
                    .combined_limit
                    .as_ref()
                    .is_some_and(expr_contains_volatile_function)
                || set_op
                    .combined_offset
                    .as_ref()
                    .is_some_and(expr_contains_volatile_function)
        })
}

fn expr_contains_volatile_function(expr: &Expr) -> bool {
    match expr {
        Expr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "random" | "nextval" | "currval" | "setval"
            ) || args.iter().any(expr_contains_volatile_function)
                || order_by
                    .iter()
                    .any(|order| expr_contains_volatile_function(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|expr| expr_contains_volatile_function(expr))
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().any(expr_contains_volatile_function)
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_contains_volatile_function(lhs) || expr_contains_volatile_function(rhs)
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expr_contains_volatile_function(inner)
        }
        Expr::Between { expr, low, high } => {
            expr_contains_volatile_function(expr)
                || expr_contains_volatile_function(low)
                || expr_contains_volatile_function(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_contains_volatile_function(expr)
                || list.iter().any(expr_contains_volatile_function)
        }
        Expr::WindowCall { args, spec, .. } => {
            args.iter().any(expr_contains_volatile_function)
                || spec
                    .partition_by
                    .iter()
                    .any(expr_contains_volatile_function)
                || spec
                    .order_by
                    .iter()
                    .any(|order| expr_contains_volatile_function(&order.expr))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_contains_volatile_function(&frame.start)
                        || frame_bound_contains_volatile_function(&frame.end)
                })
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| expr_contains_volatile_function(expr))
                || when.iter().any(|(cond, result)| {
                    expr_contains_volatile_function(cond) || expr_contains_volatile_function(result)
                })
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_contains_volatile_function(expr))
        }
        Expr::ScalarSubquery(body) => select_contains_volatile_function(body),
        Expr::Exists { body, .. } => select_contains_volatile_function(body),
        Expr::InSubquery { expr, body, .. } => {
            expr_contains_volatile_function(expr) || select_contains_volatile_function(body)
        }
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => false,
    }
}

fn frame_bound_contains_volatile_function(bound: &uqa_sql::ast::FrameBound) -> bool {
    match bound {
        uqa_sql::ast::FrameBound::Preceding(expr) | uqa_sql::ast::FrameBound::Following(expr) => {
            expr_contains_volatile_function(expr)
        }
        uqa_sql::ast::FrameBound::UnboundedPreceding
        | uqa_sql::ast::FrameBound::UnboundedFollowing
        | uqa_sql::ast::FrameBound::CurrentRow => false,
    }
}

fn frame_bound_contains_subquery(bound: &uqa_sql::ast::FrameBound) -> bool {
    match bound {
        uqa_sql::ast::FrameBound::Preceding(expr) | uqa_sql::ast::FrameBound::Following(expr) => {
            expr_contains_subquery(expr)
        }
        uqa_sql::ast::FrameBound::UnboundedPreceding
        | uqa_sql::ast::FrameBound::UnboundedFollowing
        | uqa_sql::ast::FrameBound::CurrentRow => false,
    }
}

fn frame_bound_references_outer(
    bound: &uqa_sql::ast::FrameBound,
    local_qualifiers: &std::collections::BTreeSet<String>,
    has_local_from: bool,
) -> bool {
    match bound {
        uqa_sql::ast::FrameBound::Preceding(expr) | uqa_sql::ast::FrameBound::Following(expr) => {
            expr_references_outer(expr, local_qualifiers, has_local_from)
        }
        uqa_sql::ast::FrameBound::UnboundedPreceding
        | uqa_sql::ast::FrameBound::UnboundedFollowing
        | uqa_sql::ast::FrameBound::CurrentRow => false,
    }
}

fn run_query_block(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    run_query_block_with_prepared_exists(engine, stmt, params, ctes, None)
}

fn run_query_block_with_prepared_exists(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
    ctes: &mut CteScope,
    prepared_exists_filter: Option<&ExistsMembershipPlan>,
) -> Result<SQLResult, SQLError> {
    let Some(from) = stmt.from.as_ref() else {
        return run_select_without_from(engine, stmt, params);
    };

    // `execute_select` is used for set-op branches, CTEs, and
    // derived-table bodies. Those query blocks still need the same
    // search-aware single-table path as top-level `run_select`;
    // otherwise registry-backed predicates such as
    // `fuse_log_odds(bayesian_match(...), knn_match(...))` fall
    // through to scalar expression evaluation.
    if prepared_exists_filter.is_none() {
        if let FromClause::Table { name, alias } = from {
            if alias.is_none() && engine.foreign_table(name).is_some() {
                return run_single_foreign_select(engine, name, stmt, params);
            }
            let is_virtual = name.contains('.')
                || (engine.table(name).is_none() && engine.foreign_table(name).is_none());
            let has_subquery_filter = stmt.r#where.as_ref().is_some_and(expr_contains_subquery);
            if alias.is_none()
                && !has_window(&stmt.projections)
                && !is_virtual
                && !has_subquery_filter
            {
                return run_single_table_select(engine, name, stmt, params);
            }
        }
    }

    let column_prune = column_prune_for_stmt(stmt, from);
    let qualifier_filters = qualifier_filters_for_stmt(stmt, from);
    let owned_exists_filter = if prepared_exists_filter.is_none() {
        stmt.r#where
            .as_ref()
            .map(|filter| prepare_exists_membership_filter(engine, filter, params, ctes))
            .transpose()?
            .flatten()
    } else {
        None
    };
    let exists_filter = prepared_exists_filter.or(owned_exists_filter.as_ref());
    let mut early_exists_applied = false;
    let joined = if let Some(exists_filter) = exists_filter {
        let mut row_filter = |rows: &mut Vec<ResultRow>| -> Result<(), SQLError> {
            if !early_exists_applied
                && exists_membership_plan_applicable_to_rows(exists_filter, rows)
            {
                let filtered = apply_exists_membership_filter(
                    engine,
                    std::mem::take(rows),
                    exists_filter,
                    params,
                )?;
                *rows = filtered;
                early_exists_applied = true;
            }
            Ok(())
        };
        match (column_prune.as_ref(), qualifier_filters.as_ref()) {
            (Some(prune), Some(filters)) => {
                build_join_rows_with_ctes_filtered_pruned_filtered_by_qualifier(
                    engine,
                    from,
                    params,
                    ctes,
                    &mut row_filter,
                    prune,
                    filters,
                )?
            }
            (Some(prune), None) => build_join_rows_with_ctes_filtered_pruned(
                engine,
                from,
                params,
                ctes,
                &mut row_filter,
                prune,
            )?,
            (None, Some(filters)) => build_join_rows_with_ctes_filtered_filtered_by_qualifier(
                engine,
                from,
                params,
                ctes,
                &mut row_filter,
                filters,
            )?,
            (None, None) => {
                build_join_rows_with_ctes_filtered(engine, from, params, ctes, &mut row_filter)?
            }
        }
    } else {
        match (column_prune.as_ref(), qualifier_filters.as_ref()) {
            (Some(prune), Some(filters)) => build_join_rows_with_ctes_pruned_filtered_by_qualifier(
                engine, from, params, ctes, prune, filters,
            )?,
            (Some(prune), None) => {
                build_join_rows_with_ctes_pruned(engine, from, params, ctes, prune)?
            }
            (None, Some(filters)) => build_join_rows_with_ctes_filtered_by_qualifier(
                engine, from, params, ctes, filters,
            )?,
            (None, None) => build_join_rows_with_ctes(engine, from, params, ctes)?,
        }
    };

    // Aggregates and window functions still go through their dedicated
    // routines because they need access to the SQL function registry
    // (e.g. text_match calls in projection lists). Pure projection
    // SELECTs flow through a Volcano sub-pipeline:
    //   TableScan -> [Filter] -> Project -> [Sort] -> [Limit]
    // built on the operators in `uqa-execution` so the planner-driven
    // execution layer is exercised on every projection-only SELECT.
    let final_filter =
        final_filter_after_qualifier_pushdown(stmt, from, qualifier_filters.as_ref());
    let filtered = if let Some(filter) = final_filter.as_ref() {
        if joined.is_empty() {
            joined
        } else if let Some(exists_filter) = prepared_exists_filter {
            if early_exists_applied {
                joined
            } else {
                apply_exists_membership_filter(engine, joined, exists_filter, params)?
            }
        } else if let Some(exists_filter) = owned_exists_filter.as_ref() {
            if early_exists_applied {
                joined
            } else {
                apply_exists_membership_filter(engine, joined, exists_filter, params)?
            }
        } else {
            let filter = precompute_uncorrelated_subqueries(engine, filter, params, ctes)?;
            let scoped_hook = ScopedEngineHook {
                engine,
                ctes,
                subquery_cache: RefCell::new(BTreeMap::new()),
            };
            let mut out: Vec<ResultRow> = Vec::with_capacity(joined.len());
            for row in joined {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(&scoped_hook);
                if uqa_sql::expr::eval(&filter, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v)) {
                    out.push(row);
                }
            }
            out
        }
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
        let windowed = compute_window_columns(engine, &stmt.projections, filtered, params)?;
        let mut rows: Vec<ResultRow> = windowed
            .rows
            .iter()
            .map(|src| {
                project_join_row_with_engine(Some(engine), src, &windowed.projections, params)
            })
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    // Pure projection: use the Volcano Project + Sort + Limit chain.
    let scoped_hook = ScopedEngineHook {
        engine,
        ctes,
        subquery_cache: RefCell::new(BTreeMap::new()),
    };
    let projected = volcano_project_sort_limit(engine, &filtered, stmt, params, &scoped_hook)?;
    let columns = expand_from_star_columns(
        engine,
        projection_columns(&stmt.projections),
        &stmt.projections,
        from,
    );
    Ok(SQLResult::from_rows(columns, projected))
}

fn column_prune_for_stmt(stmt: &SelectStmt, from: &FromClause) -> Option<ColumnPrune> {
    if has_window(&stmt.projections)
        || stmt.projections.iter().any(|projection| {
            matches!(projection.expr, Expr::Star)
                || expr_contains_subquery(&projection.expr)
                || expr_contains_volatile_function(&projection.expr)
        })
    {
        return None;
    }

    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    if qualifiers.is_empty() {
        return None;
    }

    let mut prune: ColumnPrune = qualifiers
        .iter()
        .map(|qualifier| (qualifier.clone(), BTreeSet::new()))
        .collect();
    let mut valid = true;
    collect_from_prune_columns(from, &qualifiers, &mut prune, &mut valid);
    for projection in &stmt.projections {
        collect_expr_prune_columns(&projection.expr, &qualifiers, &mut prune, &mut valid);
    }
    if let Some(filter) = stmt.r#where.as_ref() {
        collect_expr_prune_columns(filter, &qualifiers, &mut prune, &mut valid);
    }
    for expr in &stmt.group_by {
        collect_expr_prune_columns(expr, &qualifiers, &mut prune, &mut valid);
    }
    for set in &stmt.grouping_sets {
        for expr in set {
            collect_expr_prune_columns(expr, &qualifiers, &mut prune, &mut valid);
        }
    }
    if let Some(having) = stmt.having.as_ref() {
        collect_expr_prune_columns(having, &qualifiers, &mut prune, &mut valid);
    }
    for order in &stmt.order_by {
        collect_expr_prune_columns(&order.expr, &qualifiers, &mut prune, &mut valid);
    }
    for expr in &stmt.distinct_on {
        collect_expr_prune_columns(expr, &qualifiers, &mut prune, &mut valid);
    }
    if !valid {
        return None;
    }
    Some(prune)
}

fn collect_from_qualifiers(from: &FromClause, out: &mut Vec<String>) {
    match from {
        FromClause::Table { name, alias } => {
            out.push(alias.clone().unwrap_or_else(|| name.clone()));
        }
        FromClause::Join { left, right, .. } => {
            collect_from_qualifiers(left, out);
            collect_from_qualifiers(right, out);
        }
        FromClause::Values { alias, .. }
        | FromClause::Function { alias, .. }
        | FromClause::Subquery { alias, .. } => {
            if let Some(alias) = alias {
                out.push(alias.clone());
            }
        }
    }
}

fn collect_from_prune_columns(
    from: &FromClause,
    qualifiers: &[String],
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    match from {
        FromClause::Join {
            left, right, on, ..
        } => {
            collect_from_prune_columns(left, qualifiers, prune, valid);
            collect_from_prune_columns(right, qualifiers, prune, valid);
            if let Some(on) = on.as_ref() {
                collect_expr_prune_columns(on, qualifiers, prune, valid);
            }
        }
        FromClause::Values { rows, .. } => {
            for row in rows {
                for expr in row {
                    collect_expr_prune_columns(expr, qualifiers, prune, valid);
                }
            }
        }
        FromClause::Function { args, .. } => {
            for expr in args {
                collect_expr_prune_columns(expr, qualifiers, prune, valid);
            }
        }
        FromClause::Subquery { .. } => {
            *valid = false;
        }
        FromClause::Table { .. } => {}
    }
}

fn collect_expr_prune_columns(
    expr: &Expr,
    qualifiers: &[String],
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    match expr {
        Expr::Column(column) => {
            for qualifier in qualifiers {
                if let Some(columns) = prune.get_mut(qualifier) {
                    columns.insert(column.clone());
                }
            }
        }
        Expr::QualifiedColumn {
            qualifier, column, ..
        } => {
            if let Some(columns) = prune.get_mut(qualifier) {
                columns.insert(column.clone());
            } else {
                *valid = false;
            }
        }
        Expr::Literal(_) | Expr::Param(_) => {}
        Expr::Star | Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
            *valid = false;
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_expr_prune_columns(item, qualifiers, prune, valid);
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for arg in args {
                collect_expr_prune_columns(arg, qualifiers, prune, valid);
            }
            for order in order_by {
                collect_expr_prune_columns(&order.expr, qualifiers, prune, valid);
            }
            if let Some(filter) = filter.as_ref() {
                collect_expr_prune_columns(filter, qualifiers, prune, valid);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_prune_columns(lhs, qualifiers, prune, valid);
            collect_expr_prune_columns(rhs, qualifiers, prune, valid);
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            collect_expr_prune_columns(inner, qualifiers, prune, valid);
        }
        Expr::Between { expr, low, high } => {
            collect_expr_prune_columns(expr, qualifiers, prune, valid);
            collect_expr_prune_columns(low, qualifiers, prune, valid);
            collect_expr_prune_columns(high, qualifiers, prune, valid);
        }
        Expr::InList { expr, list, .. } => {
            collect_expr_prune_columns(expr, qualifiers, prune, valid);
            for item in list {
                collect_expr_prune_columns(item, qualifiers, prune, valid);
            }
        }
        Expr::WindowCall { .. } => {
            *valid = false;
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_ref() {
                collect_expr_prune_columns(base, qualifiers, prune, valid);
            }
            for (cond, result) in when {
                collect_expr_prune_columns(cond, qualifiers, prune, valid);
                collect_expr_prune_columns(result, qualifiers, prune, valid);
            }
            if let Some(else_branch) = else_branch.as_ref() {
                collect_expr_prune_columns(else_branch, qualifiers, prune, valid);
            }
        }
    }
}

fn qualifier_filters_for_stmt(stmt: &SelectStmt, from: &FromClause) -> Option<QualifierFilters> {
    let filter = stmt.r#where.as_ref()?;
    if expr_contains_subquery(filter) || expr_contains_volatile_function(filter) {
        return None;
    }
    let from_quals = from_qualifier_set(from);
    if from_quals.is_empty() {
        return None;
    }
    let single_qualifier = (from_quals.len() == 1)
        .then(|| from_quals.iter().next().cloned())
        .flatten();
    let mut filters = QualifierFilters::new();
    for part in flatten_and_filter_parts(filter) {
        if let Some((qualifier, filter)) =
            qualifier_filter_for_part(part, &from_quals, single_qualifier.as_deref())
        {
            filters.entry(qualifier).or_default().push(filter);
        }
    }
    (!filters.is_empty()).then_some(filters)
}

fn qualifier_filter_for_part(
    part: &Expr,
    from_quals: &BTreeSet<String>,
    single_qualifier: Option<&str>,
) -> Option<(String, Expr)> {
    if expr_contains_subquery(part) || expr_contains_volatile_function(part) {
        return None;
    }
    let qualifiers = expr_qualifiers(part);
    let has_unqualified = expr_has_unqualified_column(part);
    if qualifiers.len() == 1 && (!has_unqualified || from_quals.len() == 1) {
        let qualifier = qualifiers.iter().next().unwrap();
        if from_quals.contains(qualifier) {
            return Some((qualifier.clone(), part.clone()));
        }
    }
    if qualifiers.is_empty() && has_unqualified {
        if let Some(qualifier) = single_qualifier {
            return Some((
                qualifier.to_string(),
                qualify_unqualified_columns(part, qualifier),
            ));
        }
    }
    None
}

fn final_filter_after_qualifier_pushdown(
    stmt: &SelectStmt,
    from: &FromClause,
    filters: Option<&QualifierFilters>,
) -> Option<Expr> {
    let filter = stmt.r#where.as_ref()?;
    if filters.is_none() || !qualifier_filter_elision_safe(from) {
        return Some(filter.clone());
    }
    let from_quals = from_qualifier_set(from);
    let single_qualifier = (from_quals.len() == 1)
        .then(|| from_quals.iter().next().cloned())
        .flatten();
    let residual: Vec<Expr> = flatten_and_filter_parts(filter)
        .into_iter()
        .filter(|part| {
            qualifier_filter_for_part(part, &from_quals, single_qualifier.as_deref()).is_none()
        })
        .cloned()
        .collect();
    combine_filter_parts(residual)
}

fn qualifier_filter_elision_safe(from: &FromClause) -> bool {
    match from {
        FromClause::Join {
            left, right, kind, ..
        } => {
            matches!(
                kind,
                uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross
            ) && qualifier_filter_elision_safe(left)
                && qualifier_filter_elision_safe(right)
        }
        FromClause::Table { .. }
        | FromClause::Values { .. }
        | FromClause::Function { .. }
        | FromClause::Subquery { .. } => true,
    }
}

fn combine_filter_parts(mut parts: Vec<Expr>) -> Option<Expr> {
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Expr::And(parts)),
    }
}

fn flatten_and_filter_parts(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::And(items) => items.iter().flat_map(flatten_and_filter_parts).collect(),
        other => vec![other],
    }
}

fn from_qualifier_set(from: &FromClause) -> BTreeSet<String> {
    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    qualifiers.into_iter().collect()
}

fn expr_qualifiers(expr: &Expr) -> BTreeSet<String> {
    let mut qualifiers = BTreeSet::new();
    collect_expr_qualifiers(expr, &mut qualifiers);
    qualifiers
}

fn collect_expr_qualifiers(expr: &Expr, qualifiers: &mut BTreeSet<String>) {
    match expr {
        Expr::QualifiedColumn { qualifier, .. } => {
            qualifiers.insert(qualifier.clone());
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_expr_qualifiers(item, qualifiers);
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for arg in args {
                collect_expr_qualifiers(arg, qualifiers);
            }
            for order in order_by {
                collect_expr_qualifiers(&order.expr, qualifiers);
            }
            if let Some(filter) = filter.as_ref() {
                collect_expr_qualifiers(filter, qualifiers);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_qualifiers(lhs, qualifiers);
            collect_expr_qualifiers(rhs, qualifiers);
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            collect_expr_qualifiers(inner, qualifiers);
        }
        Expr::Between { expr, low, high } => {
            collect_expr_qualifiers(expr, qualifiers);
            collect_expr_qualifiers(low, qualifiers);
            collect_expr_qualifiers(high, qualifiers);
        }
        Expr::InList { expr, list, .. } => {
            collect_expr_qualifiers(expr, qualifiers);
            for item in list {
                collect_expr_qualifiers(item, qualifiers);
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for arg in args {
                collect_expr_qualifiers(arg, qualifiers);
            }
            for expr in &spec.partition_by {
                collect_expr_qualifiers(expr, qualifiers);
            }
            for order in &spec.order_by {
                collect_expr_qualifiers(&order.expr, qualifiers);
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_ref() {
                collect_expr_qualifiers(base, qualifiers);
            }
            for (cond, result) in when {
                collect_expr_qualifiers(cond, qualifiers);
                collect_expr_qualifiers(result, qualifiers);
            }
            if let Some(else_branch) = else_branch.as_ref() {
                collect_expr_qualifiers(else_branch, qualifiers);
            }
        }
        Expr::InSubquery { expr, .. } => collect_expr_qualifiers(expr, qualifiers),
        Expr::Column(_)
        | Expr::Literal(_)
        | Expr::Param(_)
        | Expr::Star
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => {}
    }
}

fn expr_has_unqualified_column(expr: &Expr) -> bool {
    match expr {
        Expr::Column(_) => true,
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().any(expr_has_unqualified_column)
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_has_unqualified_column)
                || order_by
                    .iter()
                    .any(|order| expr_has_unqualified_column(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|filter| expr_has_unqualified_column(filter))
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_has_unqualified_column(lhs) || expr_has_unqualified_column(rhs)
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expr_has_unqualified_column(inner)
        }
        Expr::Between { expr, low, high } => {
            expr_has_unqualified_column(expr)
                || expr_has_unqualified_column(low)
                || expr_has_unqualified_column(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_has_unqualified_column(expr) || list.iter().any(expr_has_unqualified_column)
        }
        Expr::WindowCall { args, spec, .. } => {
            args.iter().any(expr_has_unqualified_column)
                || spec.partition_by.iter().any(expr_has_unqualified_column)
                || spec
                    .order_by
                    .iter()
                    .any(|order| expr_has_unqualified_column(&order.expr))
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| expr_has_unqualified_column(expr))
                || when.iter().any(|(cond, result)| {
                    expr_has_unqualified_column(cond) || expr_has_unqualified_column(result)
                })
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_has_unqualified_column(expr))
        }
        Expr::InSubquery { expr, .. } => expr_has_unqualified_column(expr),
        Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_)
        | Expr::Star
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => false,
    }
}

fn qualify_unqualified_columns(expr: &Expr, qualifier: &str) -> Expr {
    match expr {
        Expr::Column(column) => Expr::qualified_column(qualifier, column),
        Expr::QualifiedColumn { .. } | Expr::Literal(_) | Expr::Param(_) | Expr::Star => {
            expr.clone()
        }
        Expr::Array(items) => Expr::Array(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        Expr::And(items) => Expr::And(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        Expr::Or(items) => Expr::Or(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(qualify_unqualified_columns(lhs, qualifier)),
            rhs: Box::new(qualify_unqualified_columns(rhs, qualifier)),
        },
        Expr::Not(inner) => Expr::Not(Box::new(qualify_unqualified_columns(inner, qualifier))),
        Expr::IsNull { expr, negated } => Expr::IsNull {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Expr::Between {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            low: Box::new(qualify_unqualified_columns(low, qualifier)),
            high: Box::new(qualify_unqualified_columns(high, qualifier)),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            list: list
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
            negated: *negated,
        },
        Expr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => Expr::Func {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| qualify_unqualified_columns(arg, qualifier))
                .collect(),
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_ref()
                .map(|filter| Box::new(qualify_unqualified_columns(filter, qualifier))),
        },
        Expr::WindowCall { name, args, spec } => Expr::WindowCall {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| qualify_unqualified_columns(arg, qualifier))
                .collect(),
            spec: spec.clone(),
        },
        Expr::Case {
            base,
            when,
            else_branch,
        } => Expr::Case {
            base: base
                .as_ref()
                .map(|expr| Box::new(qualify_unqualified_columns(expr, qualifier))),
            when: when
                .iter()
                .map(|(cond, result)| {
                    (
                        qualify_unqualified_columns(cond, qualifier),
                        qualify_unqualified_columns(result, qualifier),
                    )
                })
                .collect(),
            else_branch: else_branch
                .as_ref()
                .map(|expr| Box::new(qualify_unqualified_columns(expr, qualifier))),
        },
        Expr::Cast { expr, ty } => Expr::Cast {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            ty: ty.clone(),
        },
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => Expr::InSubquery {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            body: body.clone(),
            negated: *negated,
        },
        Expr::ScalarSubquery(_) | Expr::Exists { .. } => expr.clone(),
    }
}

fn expand_from_star_columns(
    engine: &Engine,
    columns: Vec<String>,
    projections: &[Projection],
    from: &FromClause,
) -> Vec<String> {
    let has_star = projections.iter().any(|p| matches!(p.expr, Expr::Star));
    if !has_star {
        return columns;
    }
    let source_cols = from_clause_output_columns(engine, from);
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

fn from_clause_output_columns(engine: &Engine, from: &FromClause) -> Vec<String> {
    match from {
        FromClause::Function {
            name,
            alias,
            column_aliases,
            ..
        } => {
            let cols = if column_aliases.is_empty() {
                user_function_output_columns(engine, name).unwrap_or_else(|| vec![name.clone()])
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
            let mut cols = from_clause_output_columns(engine, left);
            cols.extend(from_clause_output_columns(engine, right));
            cols
        }
        FromClause::Table { .. } => Vec::new(),
    }
}

/// Output column names of a user-defined routine used as a FROM
/// source: OUT / INOUT / `RETURNS TABLE` parameter names. `None` when
/// the name is not a user routine or its result is a single unnamed
/// column (which keeps the function-name default).
fn user_function_output_columns(engine: &Engine, name: &str) -> Option<Vec<String>> {
    let overloads = engine.lookup_sql_functions(name)?;
    for function in &overloads {
        let outs = function.def.output_params();
        if !outs.is_empty() {
            return Some(
                outs.iter()
                    .enumerate()
                    .map(|(idx, p)| {
                        if p.name.is_empty() {
                            format!("column{}", idx + 1)
                        } else {
                            p.name.clone()
                        }
                    })
                    .collect(),
            );
        }
    }
    None
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
    projection_hook: &dyn uqa_sql::expr::EngineHook,
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
    let has_subquery_projection = stmt
        .projections
        .iter()
        .any(|p| expr_contains_subquery(&p.expr));
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
    let top_k_rows = top_k_ordered_source_rows(
        engine,
        src_rows,
        stmt,
        params,
        resolved_offset,
        resolved_limit,
    )?;

    if !has_engine_funcs
        && !has_star
        && !has_subquery_projection
        && stmt.order_by.is_empty()
        && resolved_offset.is_none()
        && resolved_limit.is_none()
    {
        let labels = projection_columns(&stmt.projections);
        return src_rows
            .iter()
            .map(|src| {
                project_join_row_with_hook_and_labels(
                    Some(projection_hook),
                    src,
                    &stmt.projections,
                    &labels,
                    params,
                )
            })
            .collect();
    }

    if has_engine_funcs || has_star || has_subquery_projection {
        let staged = if let Some(rows) = top_k_rows {
            rows
        } else {
            stage_source_rows(
                src_rows,
                stmt,
                engine,
                params,
                resolved_offset,
                resolved_limit,
            )?
        };
        let rows: Vec<ResultRow> = staged
            .iter()
            .map(|src| {
                project_join_row_with_hook(Some(projection_hook), src, &stmt.projections, params)
            })
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
    let top_k_applied = top_k_rows.is_some();
    let source_rows = top_k_rows.unwrap_or_else(|| src_rows.to_vec());
    let mut op: Box<dyn PhysicalOperator> = Box::new(TableScan::from_rows(columns, source_rows));

    if !top_k_applied && !stmt.order_by.is_empty() {
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
    if !top_k_applied && (resolved_offset.is_some() || resolved_limit.is_some()) {
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
    engine: &Engine,
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
    if let Some(rows) = top_k_ordered_source_rows(engine, src_rows, stmt, params, offset, limit)? {
        return Ok(rows);
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

fn top_k_ordered_source_rows(
    engine: &Engine,
    src_rows: &[ResultRow],
    stmt: &SelectStmt,
    params: &[SQLParam],
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    let Some(limit) = limit else {
        return Ok(None);
    };
    if stmt.order_by.is_empty() {
        return Ok(None);
    }
    let offset = offset.unwrap_or(0) as usize;
    let limit = limit as usize;
    let keep = offset.saturating_add(limit);
    if keep == 0 {
        return Ok(Some(Vec::new()));
    }
    if keep >= src_rows.len() {
        return Ok(None);
    }

    let mut decorated = Vec::with_capacity(src_rows.len());
    for (idx, row) in src_rows.iter().enumerate() {
        let ctx = EvalContext::new(Some(row), params).with_engine(engine);
        let mut key_values = Vec::with_capacity(stmt.order_by.len());
        for order in &stmt.order_by {
            key_values.push(eval(&order.expr, &ctx)?);
        }
        decorated.push((key_values, idx, row.clone()));
    }

    decorated.select_nth_unstable_by(keep, |a, b| {
        compare_order_key_values(&a.0, a.1, &b.0, b.1, stmt)
    });
    decorated.truncate(keep);
    decorated.sort_by(|a, b| compare_order_key_values(&a.0, a.1, &b.0, b.1, stmt));
    if offset > 0 {
        decorated.drain(0..offset);
    }
    Ok(Some(
        decorated
            .into_iter()
            .map(|(_, _, row)| row)
            .collect::<Vec<_>>(),
    ))
}

fn compare_order_key_values(
    left: &[Value],
    left_idx: usize,
    right: &[Value],
    right_idx: usize,
    stmt: &SelectStmt,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (i, order) in stmt.order_by.iter().enumerate() {
        let left_null = matches!(left[i], Value::Null);
        let right_null = matches!(right[i], Value::Null);
        let nulls_first = order.nulls.map_or(order.descending, |n| {
            matches!(n, uqa_sql::ast::NullsOrder::First)
        });
        if left_null || right_null {
            let null_cmp = match (left_null, right_null) {
                (true, true) => Ordering::Equal,
                (true, false) => {
                    if nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (false, true) => {
                    if nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                (false, false) => Ordering::Equal,
            };
            if null_cmp != Ordering::Equal {
                return null_cmp;
            }
            continue;
        }
        let ord = compare_sort_values(&left[i], &right[i]);
        let ord = if order.descending { ord.reverse() } else { ord };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    left_idx.cmp(&right_idx)
}

fn compare_sort_values(left: &Value, right: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::{Equal, Greater, Less};
    match (left, right) {
        (Value::Null, Value::Null) => Equal,
        (Value::Null, _) => Less,
        (_, Value::Null) => Greater,
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Equal),
        (Value::Decimal(x), Value::Decimal(y)) => x.cmp(y),
        (Value::Decimal(x), Value::Int(y)) => x.cmp(&uqa_core::DecimalValue::from_i64(*y)),
        (Value::Int(x), Value::Decimal(y)) => uqa_core::DecimalValue::from_i64(*x).cmp(y),
        (Value::Decimal(x), Value::Float(y)) => {
            uqa_core::DecimalValue::from_f64_lossy(*y).map_or(Equal, |yd| x.cmp(&yd))
        }
        (Value::Float(x), Value::Decimal(y)) => {
            uqa_core::DecimalValue::from_f64_lossy(*x).map_or(Equal, |xd| xd.cmp(y))
        }
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Temporal(x), Value::Temporal(y)) => x.cmp(y),
        (Value::Temporal(x), Value::Str(y)) => {
            x.parse_same_kind(y).map_or(Equal, |parsed| x.cmp(&parsed))
        }
        (Value::Str(x), Value::Temporal(y)) => {
            y.parse_same_kind(x).map_or(Equal, |parsed| parsed.cmp(y))
        }
        _ => Equal,
    }
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

fn expr_contains_jsonpath_fts_match(expr: &Expr) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |part| {
        if expr_is_jsonpath_fts_match(part) {
            found = true;
        }
    });
    found
}

fn expr_is_jsonpath_fts_match(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Func { name, args, .. }
            if name.eq_ignore_ascii_case("fts_match")
                && matches!(
                    args.get(1),
                    Some(Expr::Literal(Value::Str(path))) if path.trim_start().starts_with('$')
                )
    )
}

pub(super) fn materialize_ctes(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<(), SQLError> {
    let has_statement_filter = stmt.r#where.is_some();
    let cte_filters = cte_output_filters_for_stmt(stmt);
    let has_inline_candidate = stmt
        .with
        .iter()
        .any(|cte| !cte.recursive && (cte_query_is_constant(&cte.query) || has_statement_filter));
    if !has_inline_candidate && cte_filters.is_empty() {
        return materialize_cte_list(engine, &stmt.with, params, ctes);
    }
    let ref_stats = cte_reference_stats(stmt);
    for cte in &stmt.with {
        if !cte.recursive
            && ref_stats.counts.get(&cte.name).copied().unwrap_or(0) == 1
            && !ref_stats.subquery_refs.contains(&cte.name)
            && (cte_query_is_constant(&cte.query)
                || (has_statement_filter && !select_contains_volatile_function(&cte.query)))
        {
            ctes.inlined.insert(cte.name.clone(), (*cte.query).clone());
            continue;
        }
        let rows = if cte.recursive {
            materialize_recursive_cte(engine, cte, params, ctes, cte_filters.get(&cte.name))?
        } else {
            let result = execute_select(engine, &cte.query, params, ctes)?;
            apply_cte_column_aliases(result.rows, &result.columns, &cte.columns)
        };
        ctes.insert_materialized(cte.name.clone(), rows);
    }
    Ok(())
}

fn cte_query_is_constant(stmt: &SelectStmt) -> bool {
    stmt.with.is_empty()
        && stmt.set_op.is_none()
        && stmt.from.is_none()
        && stmt.r#where.is_none()
        && stmt.group_by.is_empty()
        && stmt.grouping_sets.is_empty()
        && stmt.having.is_none()
        && stmt.order_by.is_empty()
        && stmt.limit.is_none()
        && stmt.offset.is_none()
        && !stmt.distinct
        && stmt.distinct_on.is_empty()
        && !stmt.projections.iter().any(|projection| {
            expr_contains_subquery(&projection.expr)
                || expr_contains_volatile_function(&projection.expr)
                || has_window_projection(&projection.expr)
        })
}

fn has_window_projection(expr: &Expr) -> bool {
    match expr {
        Expr::WindowCall { .. } => true,
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().any(has_window_projection)
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(has_window_projection)
                || order_by
                    .iter()
                    .any(|order| has_window_projection(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|expr| has_window_projection(expr))
        }
        Expr::Binary { lhs, rhs, .. } => has_window_projection(lhs) || has_window_projection(rhs),
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            has_window_projection(inner)
        }
        Expr::Between { expr, low, high } => {
            has_window_projection(expr) || has_window_projection(low) || has_window_projection(high)
        }
        Expr::InList { expr, list, .. } => {
            has_window_projection(expr) || list.iter().any(has_window_projection)
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| has_window_projection(expr))
                || when.iter().any(|(cond, result)| {
                    has_window_projection(cond) || has_window_projection(result)
                })
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| has_window_projection(expr))
        }
        Expr::InSubquery { expr, .. } => has_window_projection(expr),
        Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => false,
    }
}

pub(super) fn materialize_cte_list(
    engine: &Engine,
    list: &[CTE],
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<(), SQLError> {
    for cte in list {
        let rows = if cte.recursive {
            materialize_recursive_cte(engine, cte, params, ctes, None)?
        } else {
            let result = execute_select(engine, &cte.query, params, ctes)?;
            apply_cte_column_aliases(result.rows, &result.columns, &cte.columns)
        };
        ctes.insert_materialized(cte.name.clone(), rows);
    }
    Ok(())
}

struct CteReferenceStats {
    counts: BTreeMap<String, usize>,
    subquery_refs: HashSet<String>,
}

fn cte_reference_stats(stmt: &SelectStmt) -> CteReferenceStats {
    let names: HashSet<String> = stmt.with.iter().map(|cte| cte.name.clone()).collect();
    let mut stats = CteReferenceStats {
        counts: BTreeMap::new(),
        subquery_refs: HashSet::new(),
    };
    if names.is_empty() {
        return stats;
    }
    for cte in &stmt.with {
        collect_select_cte_references(&cte.query, &names, &mut stats, false, true);
    }
    collect_select_cte_references(stmt, &names, &mut stats, false, false);
    stats
}

fn collect_select_cte_references(
    stmt: &SelectStmt,
    names: &HashSet<String>,
    stats: &mut CteReferenceStats,
    inside_subquery: bool,
    include_with: bool,
) {
    if include_with {
        for cte in &stmt.with {
            collect_select_cte_references(&cte.query, names, stats, inside_subquery, true);
        }
    }
    if let Some(set_op) = stmt.set_op.as_ref().filter(|set_op| set_op.left.is_some()) {
        collect_select_cte_references(
            set_op.left.as_deref().unwrap(),
            names,
            stats,
            inside_subquery,
            true,
        );
        collect_select_cte_references(&set_op.right, names, stats, inside_subquery, true);
        for order in &set_op.combined_order_by {
            collect_expr_cte_references(&order.expr, names, stats, inside_subquery);
        }
        if let Some(limit) = set_op.combined_limit.as_ref() {
            collect_expr_cte_references(limit, names, stats, inside_subquery);
        }
        if let Some(offset) = set_op.combined_offset.as_ref() {
            collect_expr_cte_references(offset, names, stats, inside_subquery);
        }
        return;
    }
    if let Some(from) = stmt.from.as_ref() {
        collect_from_cte_references(from, names, stats, inside_subquery);
    }
    for projection in &stmt.projections {
        collect_expr_cte_references(&projection.expr, names, stats, inside_subquery);
    }
    if let Some(filter) = stmt.r#where.as_ref() {
        collect_expr_cte_references(filter, names, stats, inside_subquery);
    }
    for expr in &stmt.group_by {
        collect_expr_cte_references(expr, names, stats, inside_subquery);
    }
    for set in &stmt.grouping_sets {
        for expr in set {
            collect_expr_cte_references(expr, names, stats, inside_subquery);
        }
    }
    if let Some(having) = stmt.having.as_ref() {
        collect_expr_cte_references(having, names, stats, inside_subquery);
    }
    for order in &stmt.order_by {
        collect_expr_cte_references(&order.expr, names, stats, inside_subquery);
    }
    if let Some(limit) = stmt.limit.as_ref() {
        collect_expr_cte_references(limit, names, stats, inside_subquery);
    }
    if let Some(offset) = stmt.offset.as_ref() {
        collect_expr_cte_references(offset, names, stats, inside_subquery);
    }
    for expr in &stmt.distinct_on {
        collect_expr_cte_references(expr, names, stats, inside_subquery);
    }
    if let Some(set_op) = stmt.set_op.as_ref() {
        if let Some(left) = set_op.left.as_deref() {
            collect_select_cte_references(left, names, stats, inside_subquery, true);
        }
        collect_select_cte_references(&set_op.right, names, stats, inside_subquery, true);
        for order in &set_op.combined_order_by {
            collect_expr_cte_references(&order.expr, names, stats, inside_subquery);
        }
        if let Some(limit) = set_op.combined_limit.as_ref() {
            collect_expr_cte_references(limit, names, stats, inside_subquery);
        }
        if let Some(offset) = set_op.combined_offset.as_ref() {
            collect_expr_cte_references(offset, names, stats, inside_subquery);
        }
    }
}

fn collect_from_cte_references(
    from: &FromClause,
    names: &HashSet<String>,
    stats: &mut CteReferenceStats,
    inside_subquery: bool,
) {
    match from {
        FromClause::Table { name, .. } => {
            if names.contains(name) {
                *stats.counts.entry(name.clone()).or_insert(0) += 1;
                if inside_subquery {
                    stats.subquery_refs.insert(name.clone());
                }
            }
        }
        FromClause::Join {
            left, right, on, ..
        } => {
            collect_from_cte_references(left, names, stats, inside_subquery);
            collect_from_cte_references(right, names, stats, inside_subquery);
            if let Some(on) = on.as_ref() {
                collect_expr_cte_references(on, names, stats, inside_subquery);
            }
        }
        FromClause::Values { rows, .. } => {
            for row in rows {
                for expr in row {
                    collect_expr_cte_references(expr, names, stats, inside_subquery);
                }
            }
        }
        FromClause::Function { args, .. } => {
            for expr in args {
                collect_expr_cte_references(expr, names, stats, inside_subquery);
            }
        }
        FromClause::Subquery { body, .. } => {
            collect_select_cte_references(body, names, stats, true, true);
        }
    }
}

fn collect_expr_cte_references(
    expr: &Expr,
    names: &HashSet<String>,
    stats: &mut CteReferenceStats,
    inside_subquery: bool,
) {
    match expr {
        Expr::ScalarSubquery(body) | Expr::Exists { body, .. } => {
            collect_select_cte_references(body, names, stats, true, true);
        }
        Expr::InSubquery { expr, body, .. } => {
            collect_expr_cte_references(expr, names, stats, inside_subquery);
            collect_select_cte_references(body, names, stats, true, true);
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_expr_cte_references(item, names, stats, inside_subquery);
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for arg in args {
                collect_expr_cte_references(arg, names, stats, inside_subquery);
            }
            for order in order_by {
                collect_expr_cte_references(&order.expr, names, stats, inside_subquery);
            }
            if let Some(filter) = filter.as_ref() {
                collect_expr_cte_references(filter, names, stats, inside_subquery);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_cte_references(lhs, names, stats, inside_subquery);
            collect_expr_cte_references(rhs, names, stats, inside_subquery);
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            collect_expr_cte_references(inner, names, stats, inside_subquery);
        }
        Expr::Between { expr, low, high } => {
            collect_expr_cte_references(expr, names, stats, inside_subquery);
            collect_expr_cte_references(low, names, stats, inside_subquery);
            collect_expr_cte_references(high, names, stats, inside_subquery);
        }
        Expr::InList { expr, list, .. } => {
            collect_expr_cte_references(expr, names, stats, inside_subquery);
            for item in list {
                collect_expr_cte_references(item, names, stats, inside_subquery);
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for arg in args {
                collect_expr_cte_references(arg, names, stats, inside_subquery);
            }
            for expr in &spec.partition_by {
                collect_expr_cte_references(expr, names, stats, inside_subquery);
            }
            for order in &spec.order_by {
                collect_expr_cte_references(&order.expr, names, stats, inside_subquery);
            }
            if let Some(frame) = spec.frame.as_ref() {
                collect_frame_bound_cte_references(&frame.start, names, stats, inside_subquery);
                collect_frame_bound_cte_references(&frame.end, names, stats, inside_subquery);
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_ref() {
                collect_expr_cte_references(base, names, stats, inside_subquery);
            }
            for (cond, result) in when {
                collect_expr_cte_references(cond, names, stats, inside_subquery);
                collect_expr_cte_references(result, names, stats, inside_subquery);
            }
            if let Some(else_branch) = else_branch.as_ref() {
                collect_expr_cte_references(else_branch, names, stats, inside_subquery);
            }
        }
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => {}
    }
}

fn collect_frame_bound_cte_references(
    bound: &uqa_sql::ast::FrameBound,
    names: &HashSet<String>,
    stats: &mut CteReferenceStats,
    inside_subquery: bool,
) {
    match bound {
        uqa_sql::ast::FrameBound::Preceding(expr) | uqa_sql::ast::FrameBound::Following(expr) => {
            collect_expr_cte_references(expr, names, stats, inside_subquery);
        }
        uqa_sql::ast::FrameBound::UnboundedPreceding
        | uqa_sql::ast::FrameBound::UnboundedFollowing
        | uqa_sql::ast::FrameBound::CurrentRow => {}
    }
}

fn cte_output_filters_for_stmt(stmt: &SelectStmt) -> BTreeMap<String, (String, Expr)> {
    let Some(filter) = stmt.r#where.as_ref() else {
        return BTreeMap::new();
    };
    if expr_contains_subquery(filter) || expr_contains_volatile_function(filter) {
        return BTreeMap::new();
    }
    let cte_names: BTreeSet<String> = stmt.with.iter().map(|cte| cte.name.clone()).collect();
    if cte_names.is_empty() {
        return BTreeMap::new();
    }
    let mut alias_to_cte = BTreeMap::new();
    if let Some(from) = stmt.from.as_ref() {
        collect_cte_reference_aliases(from, &cte_names, &mut alias_to_cte);
    }
    if alias_to_cte.is_empty() {
        return BTreeMap::new();
    }

    let mut grouped: BTreeMap<String, (String, Vec<Expr>)> = BTreeMap::new();
    for part in flatten_and_filter_parts(filter) {
        if expr_contains_subquery(part) || expr_contains_volatile_function(part) {
            continue;
        }
        let qualifiers = expr_qualifiers(part);
        if qualifiers.len() == 1 {
            let qualifier = qualifiers.iter().next().unwrap();
            if let Some(cte_name) = alias_to_cte.get(qualifier) {
                let entry = grouped
                    .entry(cte_name.clone())
                    .or_insert_with(|| (qualifier.clone(), Vec::new()));
                entry.1.push(part.clone());
            }
            continue;
        }
        if qualifiers.is_empty() && expr_has_unqualified_column(part) && alias_to_cte.len() == 1 {
            let (qualifier, cte_name) = alias_to_cte.iter().next().unwrap();
            let entry = grouped
                .entry(cte_name.clone())
                .or_insert_with(|| (qualifier.clone(), Vec::new()));
            entry.1.push(qualify_unqualified_columns(part, qualifier));
        }
    }

    grouped
        .into_iter()
        .map(|(cte_name, (qualifier, filters))| {
            (cte_name, (qualifier, combine_filter_exprs(filters)))
        })
        .collect()
}

fn collect_cte_reference_aliases(
    from: &FromClause,
    cte_names: &BTreeSet<String>,
    out: &mut BTreeMap<String, String>,
) {
    match from {
        FromClause::Table { name, alias } => {
            if cte_names.contains(name) {
                out.insert(alias.clone().unwrap_or_else(|| name.clone()), name.clone());
            }
        }
        FromClause::Join { left, right, .. } => {
            collect_cte_reference_aliases(left, cte_names, out);
            collect_cte_reference_aliases(right, cte_names, out);
        }
        FromClause::Values { .. } | FromClause::Function { .. } => {}
        FromClause::Subquery { body, .. } => {
            if let Some(from) = body.from.as_ref() {
                collect_cte_reference_aliases(from, cte_names, out);
            }
        }
    }
}

fn combine_filter_exprs(mut filters: Vec<Expr>) -> Expr {
    if filters.len() == 1 {
        filters.pop().unwrap()
    } else {
        Expr::And(filters)
    }
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
    ctes: &mut CteScope,
    output_filter: Option<&(String, Expr)>,
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

    // Anchor: the explicit LHS subtree. Older serialized statements may not
    // carry it, so retain the historical implicit-LHS fallback.
    let mut anchor_stmt = if let Some(left) = set_op.left.as_deref() {
        left.clone()
    } else {
        let mut anchor = cte.query.as_ref().clone();
        anchor.set_op = None;
        anchor
    };
    anchor_stmt.with.clear();
    let source_anchor_columns = projection_columns(&anchor_stmt.projections);
    let anchor_columns = if cte.columns.is_empty() {
        source_anchor_columns.clone()
    } else {
        cte.columns.clone()
    };
    if let Some((qualifier, filter)) = output_filter {
        if let Some(filtered) = super::from_rows::push_output_filter_into_select_with_columns(
            engine,
            anchor_stmt.clone(),
            qualifier,
            filter,
            &anchor_columns,
        ) {
            anchor_stmt = filtered;
        }
    }
    let anchor_rows = run_query_block(engine, &anchor_stmt, params, ctes)?.rows;
    let anchor_rows =
        apply_cte_column_aliases(anchor_rows, &source_anchor_columns, &anchor_columns);

    let mut working = anchor_rows;

    let mut step_stmt = set_op.right.clone();
    step_stmt.with.clear();
    let step_columns = projection_columns(&step_stmt.projections);
    if let Some((qualifier, filter)) = output_filter {
        if let Some(filtered) = super::from_rows::push_output_filter_into_select_with_columns(
            engine,
            step_stmt.clone(),
            qualifier,
            filter,
            &anchor_columns,
        ) {
            step_stmt = filtered;
        }
    }
    let step_exists_filter = step_stmt
        .r#where
        .as_ref()
        .filter(|filter| exists_membership_filter_is_stable(engine, filter, ctes))
        .map(|filter| prepare_exists_membership_filter(engine, filter, params, ctes))
        .transpose()?
        .flatten();

    const MAX_ITER: usize = 1024;
    if set_op.all {
        let mut chunks: Vec<Vec<ResultRow>> = Vec::new();
        for _ in 0..MAX_ITER {
            if working.is_empty() {
                break;
            }
            ctes.insert_materialized(cte.name.clone(), working);
            let new_rows = run_query_block_with_prepared_exists(
                engine,
                &step_stmt,
                params,
                ctes,
                step_exists_filter.as_ref(),
            );
            let old_working = ctes.remove_materialized(&cte.name).unwrap_or_default();
            chunks.push(old_working);
            let new_rows = new_rows?.rows;

            if new_rows.is_empty() {
                working = Vec::new();
                break;
            }
            working = if step_columns == anchor_columns {
                new_rows
            } else {
                new_rows
                    .into_iter()
                    .map(|row| rename_columns(&row, &step_columns, &anchor_columns))
                    .collect()
            };
        }
        if !working.is_empty() {
            chunks.push(working);
        }
        let row_count = chunks.iter().map(Vec::len).sum();
        let mut rows = Vec::with_capacity(row_count);
        for chunk in chunks {
            rows.extend(chunk);
        }
        return Ok(rows);
    }

    let mut all_rows = working.clone();
    for _ in 0..MAX_ITER {
        if working.is_empty() {
            break;
        }
        // Bind the CTE name to the working set under the anchor's
        // column shape so the recursive step's FROM ... <cte> ... sees
        // the same keys it saw on the prior pass.
        ctes.insert_materialized(cte.name.clone(), working);
        let new_rows = run_query_block_with_prepared_exists(
            engine,
            &step_stmt,
            params,
            ctes,
            step_exists_filter.as_ref(),
        );
        ctes.remove_materialized(&cte.name);
        let new_rows = new_rows?.rows;

        if new_rows.is_empty() {
            break;
        }
        // Rename the step's positional projection labels to the
        // anchor's so subsequent iterations and the outer SELECT see a
        // consistent shape (anchor names win, mirroring `PostgreSQL`).
        let renamed: Vec<ResultRow> = if step_columns == anchor_columns {
            new_rows
        } else {
            new_rows
                .into_iter()
                .map(|row| rename_columns(&row, &step_columns, &anchor_columns))
                .collect()
        };
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
    if let Some(filter) = stmt.r#where.as_ref() {
        super::validate_expr_text_match_fields(engine, table, filter)?;
    }
    // `SELECT count(*) FROM t` with no WHERE: answer from the doc
    // count without touching doc ids or documents.
    if stmt.r#where.is_none()
        && stmt.group_by.is_empty()
        && stmt.grouping_sets.is_empty()
        && stmt.having.is_none()
        && stmt.order_by.is_empty()
        && stmt.limit.is_none()
        && stmt.offset.is_none()
        && !stmt.distinct
        && stmt.projections.len() == 1
    {
        if let Expr::Func {
            name,
            args,
            distinct,
            filter,
            ..
        } = &stmt.projections[0].expr
        {
            if name.eq_ignore_ascii_case("count")
                && matches!(args.as_slice(), [Expr::Star])
                && !*distinct
                && filter.is_none()
            {
                let columns = projection_columns(&stmt.projections);
                let mut row = ResultRow::new();
                row.insert(
                    columns[0].clone(),
                    Value::Int(engine.table_doc_count(table) as i64),
                );
                return Ok(SQLResult::from_rows(columns, vec![row]));
            }
        }
    }

    let score_top_k = score_order_top_k(stmt, engine, params)?;
    let score_top_k = score_top_k.filter(|_| score_limited_text_filter(stmt.r#where.as_ref()));
    let has_jsonpath_fts_filter = stmt
        .r#where
        .as_ref()
        .is_some_and(expr_contains_jsonpath_fts_match);
    // Try the operator-tree pipeline first: lower the WHERE clause to
    // an `OperatorTree`, run `QueryOptimizer` (10 algebraic / graph-
    // aware / fusion-reordering passes - compatibility), then execute
    // through `PlanExecutor` against an `EngineDriver`. The bridge
    // returns `None` for shapes the operator IR can't represent
    // (arithmetic across columns, sub-queries, window calls, ...) and
    // we fall back to the legacy direct dispatch in that case.
    let optimised = if has_jsonpath_fts_filter {
        None
    } else if let (Some(top_k), Some(Expr::Func { name, args, .. })) =
        (score_top_k, stmt.r#where.as_ref())
    {
        Some(execute_function_with_top_k(
            engine,
            table,
            name,
            args,
            params,
            Some(top_k),
        )?)
    } else {
        crate::operator_tree_bridge::run_optimised(engine, table, stmt.r#where.as_ref(), params)?
    };
    let scored = if let Some(rows) = optimised {
        rows
    } else {
        match stmt.r#where.as_ref() {
            None => engine
                .table_doc_ids(table)
                .into_iter()
                .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
                .collect::<Vec<_>>(),
            Some(filter_expr @ Expr::Func { name, args, .. })
                if uqa_sql::registry::is_registered(name)
                    && !expr_is_jsonpath_fts_match(filter_expr) =>
            {
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
        // ORDER BY references something other than `_score`. When the
        // order keys resolve against stored document fields alone
        // (no projection aliases, no window/aggregate output), order
        // doc ids first and project only the surviving rows - with a
        // LIMIT this turns a full projection pass into a top-K
        // selection.
        if let Some(result) = run_doc_ordered_select(engine, table, &scored, stmt, params)? {
            return Ok(result);
        }
        // Fallback: ORDER BY needs projected values (aliases,
        // computed columns). Project every row, merge the underlying
        // document fields in the same pass so the row evaluator can
        // read columns the projection dropped, then order and strip.
        let columns = projection_columns(&stmt.projections);
        let doc_ids: Vec<DocId> = scored.iter().map(|entry| entry.doc_id).collect();
        let documents = engine.get_documents_bulk(table, &doc_ids);
        let mut all_rows = Vec::with_capacity(scored.len());
        for entry in &scored {
            let mut document = documents.get(&entry.doc_id).cloned().unwrap_or_default();
            document.insert(DOC_ID_COLUMN.into(), Value::Int(entry.doc_id as i64));
            document.insert(SCORE_COLUMN.into(), Value::Float(entry.score));
            let mut row = build_projection_row(Some(engine), &document, &stmt.projections, params)?;
            for (k, v) in document {
                if k.as_str() == DOC_ID_COLUMN || k.as_str() == SCORE_COLUMN {
                    continue;
                }
                row.entry(k).or_insert(v);
            }
            all_rows.push(row);
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
        let windowed = compute_window_columns(engine, &stmt.projections, filtered, params)?;
        let mut rows: Vec<ResultRow> = windowed
            .rows
            .iter()
            .map(|src| {
                project_join_row_with_engine(Some(engine), src, &windowed.projections, params)
            })
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let rows = volcano_project_sort_limit(engine, &filtered, stmt, params, engine)?;
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
        Expr::Column(name) => name != SCORE_COLUMN,
        _ => true,
    })
}

/// Collect bare column names referenced by an ORDER BY expression.
/// Returns `false` (ineligible) when the expression contains anything
/// that cannot be resolved against a stored document alone: function
/// calls, subqueries, window calls, `*`, or a bare literal (which
/// `PostgreSQL` would treat as an output-ordinal reference).
fn collect_order_key_columns(expr: &Expr, out: &mut Vec<String>) -> bool {
    match expr {
        Expr::Column(name) => {
            out.push(name.clone());
            true
        }
        Expr::QualifiedColumn { column, .. } => {
            out.push(column.clone());
            true
        }
        Expr::Literal(_) | Expr::Param(_) => true,
        Expr::Binary { lhs, rhs, .. } => {
            collect_order_key_columns(lhs, out) && collect_order_key_columns(rhs, out)
        }
        Expr::Not(inner) | Expr::Cast { expr: inner, .. } => collect_order_key_columns(inner, out),
        Expr::IsNull { expr, .. } => collect_order_key_columns(expr, out),
        Expr::Between { expr, low, high } => {
            collect_order_key_columns(expr, out)
                && collect_order_key_columns(low, out)
                && collect_order_key_columns(high, out)
        }
        Expr::InList { expr, list, .. } => {
            collect_order_key_columns(expr, out)
                && list.iter().all(|item| collect_order_key_columns(item, out))
        }
        Expr::And(items) | Expr::Or(items) | Expr::Array(items) => items
            .iter()
            .all(|item| collect_order_key_columns(item, out)),
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref()
                .is_none_or(|b| collect_order_key_columns(b, out))
                && when.iter().all(|(condition, result)| {
                    collect_order_key_columns(condition, out)
                        && collect_order_key_columns(result, out)
                })
                && else_branch
                    .as_deref()
                    .is_none_or(|e| collect_order_key_columns(e, out))
        }
        _ => false,
    }
}

/// Fast path for `ORDER BY <document fields> [LIMIT k]`: evaluate the
/// order keys straight off the stored documents, keep only the rows
/// that survive OFFSET/LIMIT, and run the projection on those rows
/// alone. Returns `Ok(None)` when an order key needs projected values
/// (aliases, ordinals, functions, subqueries) so the caller can use
/// the project-first fallback.
fn run_doc_ordered_select(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<Option<SQLResult>, SQLError> {
    use uqa_execution::relational::{compare_sort_key_values, SortKey};

    let columns = projection_columns(&stmt.projections);
    // Ordinal ORDER BY (bare integer literal) refers to the projected
    // output; leave it to the fallback.
    if stmt
        .order_by
        .iter()
        .any(|o| matches!(o.expr, Expr::Literal(_)))
    {
        return Ok(None);
    }
    let mut referenced = Vec::new();
    for order in &stmt.order_by {
        if !collect_order_key_columns(&order.expr, &mut referenced) {
            return Ok(None);
        }
    }
    // A referenced name that matches a projection alias must resolve
    // to the projected value under PostgreSQL scoping rules - only
    // safe here when the alias is the same bare column.
    for name in &referenced {
        if name == SCORE_COLUMN || name == DOC_ID_COLUMN {
            continue;
        }
        for (idx, label) in columns.iter().enumerate() {
            if label == name && !matches!(&stmt.projections[idx].expr, Expr::Column(c) if c == name)
            {
                return Ok(None);
            }
        }
    }
    let needs_score = referenced
        .iter()
        .any(|name| name == SCORE_COLUMN || name == DOC_ID_COLUMN);

    let resolved_offset =
        resolve_limit_offset(stmt.offset.as_ref(), engine, params, "OFFSET")?.unwrap_or(0) as usize;
    let resolved_limit = resolve_limit_offset(stmt.limit.as_ref(), engine, params, "LIMIT")?
        .map(|limit| limit as usize);

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

    // Order keys read a known column set: fetch just those fields in
    // one storage scan instead of materialising whole documents.
    let doc_ids: Vec<DocId> = scored.iter().map(|entry| entry.doc_id).collect();
    let mut key_fields: Vec<String> = referenced
        .iter()
        .filter(|name| name.as_str() != SCORE_COLUMN && name.as_str() != DOC_ID_COLUMN)
        .cloned()
        .collect();
    key_fields.sort();
    key_fields.dedup();
    let field_refs: Vec<&str> = key_fields.iter().map(String::as_str).collect();
    let field_values = engine.get_document_fields_multi(table, &doc_ids, &field_refs);

    // Bare-column order keys read straight out of the fetched field
    // vectors; only computed keys (expressions over columns) pay for a
    // per-row document and the expression evaluator.
    enum OrderKeySource {
        Field(usize),
        Score,
        DocId,
    }
    let direct_sources: Option<Vec<OrderKeySource>> = keys
        .iter()
        .map(|key| match &key.expr {
            Expr::Column(name) if name == SCORE_COLUMN => Some(OrderKeySource::Score),
            Expr::Column(name) if name == DOC_ID_COLUMN => Some(OrderKeySource::DocId),
            Expr::Column(name) => key_fields
                .iter()
                .position(|field| field == name)
                .map(OrderKeySource::Field),
            _ => None,
        })
        .collect();

    let mut decorated: Vec<(Vec<Value>, usize)> = Vec::with_capacity(scored.len());
    if let Some(sources) = direct_sources {
        for (idx, entry) in scored.iter().enumerate() {
            let values = field_values.get(&entry.doc_id);
            let key_vals: Vec<Value> = sources
                .iter()
                .map(|source| match source {
                    OrderKeySource::Field(i) => values
                        .and_then(|row| row.get(*i))
                        .cloned()
                        .unwrap_or(Value::Null),
                    OrderKeySource::Score => Value::Float(entry.score),
                    OrderKeySource::DocId => Value::Int(entry.doc_id as i64),
                })
                .collect();
            decorated.push((key_vals, idx));
        }
    } else {
        for (idx, entry) in scored.iter().enumerate() {
            let mut doc = Document::new();
            if let Some(values) = field_values.get(&entry.doc_id) {
                for (name, value) in key_fields.iter().zip(values) {
                    doc.insert(name.clone(), value.clone());
                }
            }
            if needs_score {
                doc.insert(DOC_ID_COLUMN.into(), Value::Int(entry.doc_id as i64));
                doc.insert(SCORE_COLUMN.into(), Value::Float(entry.score));
            }
            let ctx = EvalContext::new(Some(&doc), params).with_engine(engine);
            let mut key_vals = Vec::with_capacity(keys.len());
            for key in &keys {
                key_vals.push(eval(&key.expr, &ctx)?);
            }
            decorated.push((key_vals, idx));
        }
    }

    // Partial selection when a LIMIT bounds the survivors, then a
    // stable sort so equal keys keep their incoming doc-id order.
    if let Some(limit) = resolved_limit {
        let keep = resolved_offset.saturating_add(limit);
        if keep < decorated.len() {
            if keep == 0 {
                decorated.clear();
            } else {
                decorated.select_nth_unstable_by(keep - 1, |(av, ai), (bv, bi)| {
                    compare_sort_key_values(&keys, av, bv).then_with(|| ai.cmp(bi))
                });
                decorated.truncate(keep);
            }
        }
    }
    decorated.sort_by(|(av, ai), (bv, bi)| {
        compare_sort_key_values(&keys, av, bv).then_with(|| ai.cmp(bi))
    });
    if resolved_offset > 0 {
        let off = resolved_offset.min(decorated.len());
        decorated.drain(0..off);
    }
    if let Some(limit) = resolved_limit {
        decorated.truncate(limit);
    }

    // Materialise full documents only for the surviving rows.
    let survivor_ids: Vec<DocId> = decorated
        .iter()
        .map(|(_, idx)| scored[*idx].doc_id)
        .collect();
    let documents = engine.get_documents_bulk(table, &survivor_ids);
    let mut rows = Vec::with_capacity(decorated.len());
    for (_, idx) in decorated {
        let entry = &scored[idx];
        let mut document = documents.get(&entry.doc_id).cloned().unwrap_or_default();
        document.insert(DOC_ID_COLUMN.into(), Value::Int(entry.doc_id as i64));
        document.insert(SCORE_COLUMN.into(), Value::Float(entry.score));
        rows.push(build_projection_row(
            Some(engine),
            &document,
            &stmt.projections,
            params,
        )?);
    }
    Ok(Some(SQLResult::from_rows(columns, rows)))
}

fn score_limited_text_filter(expr: Option<&Expr>) -> bool {
    let Some(Expr::Func { name, .. }) = expr else {
        return false;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "text_match" | "bayesian_match"
    )
}

fn score_order_top_k(
    stmt: &SelectStmt,
    engine: &Engine,
    params: &[SQLParam],
) -> Result<Option<usize>, SQLError> {
    if stmt.distinct
        || !stmt.distinct_on.is_empty()
        || stmt.order_by.is_empty()
        || order_by_references_field(stmt)
        || stmt.order_by.iter().any(|order| !order.descending)
        || has_aggregate(engine, &stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        return Ok(None);
    }
    let Some(limit) = resolve_limit_offset(stmt.limit.as_ref(), engine, params, "LIMIT")? else {
        return Ok(None);
    };
    let offset = resolve_limit_offset(stmt.offset.as_ref(), engine, params, "OFFSET")?.unwrap_or(0);
    let top_k = usize::try_from(limit.saturating_add(offset)).unwrap_or(usize::MAX);
    Ok(Some(top_k))
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
    if let Some(filter) = stmt.r#where.as_ref() {
        super::validate_joined_expr_text_match_fields(engine, from, filter)?;
    }
    let mut ctes = CteScope::new();
    let column_prune = column_prune_for_stmt(stmt, from);
    let qualifier_filters = qualifier_filters_for_stmt(stmt, from);
    let mut joined = match (column_prune.as_ref(), qualifier_filters.as_ref()) {
        (Some(prune), Some(filters)) => build_join_rows_with_ctes_pruned_filtered_by_qualifier(
            engine, from, params, &mut ctes, prune, filters,
        )?,
        (Some(prune), None) => {
            build_join_rows_with_ctes_pruned(engine, from, params, &mut ctes, prune)?
        }
        (None, Some(filters)) => build_join_rows_with_ctes_filtered_by_qualifier(
            engine, from, params, &mut ctes, filters,
        )?,
        (None, None) => build_join_rows_with_ctes(engine, from, params, &mut ctes)?,
    };

    let final_filter =
        final_filter_after_qualifier_pushdown(stmt, from, qualifier_filters.as_ref());
    if let Some(filter) = final_filter.as_ref() {
        if joined.is_empty() {
            // No row means no row-level predicate evaluation.
        } else if let Some(exists_filter) =
            prepare_exists_membership_filter(engine, filter, params, &mut ctes)?
        {
            joined = apply_exists_membership_filter(engine, joined, &exists_filter, params)?;
        } else if expr_contains_subquery(filter) {
            let filter = precompute_uncorrelated_subqueries(engine, filter, params, &ctes)?;
            let scoped_hook = ScopedEngineHook {
                engine,
                ctes: &ctes,
                subquery_cache: RefCell::new(BTreeMap::new()),
            };
            joined.retain(|row| {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(&scoped_hook);
                uqa_sql::expr::eval(&filter, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v))
            });
        } else {
            joined.retain(|row| {
                let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
                uqa_sql::expr::eval(filter, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v))
            });
        }
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
        let windowed = compute_window_columns(engine, &stmt.projections, joined, params)?;
        let mut rows: Vec<ResultRow> = windowed
            .rows
            .iter()
            .map(|src| {
                project_join_row_with_engine(Some(engine), src, &windowed.projections, params)
            })
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let mut rows: Vec<ResultRow> = joined
        .iter()
        .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
        .collect::<Result<_, _>>()?;
    rows = apply_row_order_limit(rows, stmt, engine, params)?;
    let columns = expand_from_star_columns(
        engine,
        projection_columns(&stmt.projections),
        &stmt.projections,
        from,
    );
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

    const ORDER_KEY_PREFIX: &str = "__uqa_order_key_";

    if rows.is_empty() {
        return Ok(rows);
    }
    let resolved_offset = resolve_limit_offset(stmt.offset.as_ref(), engine, params, "OFFSET")?;
    let resolved_limit = resolve_limit_offset(stmt.limit.as_ref(), engine, params, "LIMIT")?;
    if stmt.order_by.is_empty() && resolved_offset.is_none() && resolved_limit.is_none() {
        return Ok(rows);
    }

    // Materialise ORDER BY keys before entering the Volcano pipeline:
    // the Sort operator evaluates expressions without an engine hook,
    // so registered scalar functions would fail inside it.
    let mut rows = rows;
    if !stmt.order_by.is_empty() {
        let key_values: Vec<Vec<Value>> = rows
            .iter()
            .map(|row| {
                let ctx = EvalContext::new(Some(row), params).with_engine(engine);
                stmt.order_by
                    .iter()
                    .map(|order| eval(&order.expr, &ctx))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<_, _>>()?;
        for (row, keys) in rows.iter_mut().zip(key_values) {
            for (idx, value) in keys.into_iter().enumerate() {
                row.insert(format!("{ORDER_KEY_PREFIX}{idx}"), value);
            }
        }
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
            .enumerate()
            .map(|(idx, o)| SortKey {
                expr: Expr::Column(format!("{ORDER_KEY_PREFIX}{idx}")),
                descending: o.descending,
                nulls_first: o
                    .nulls
                    .map(|n| matches!(n, uqa_sql::ast::NullsOrder::First)),
            })
            .collect();
        // With a LIMIT the sort only has to surface the first
        // OFFSET + LIMIT rows; the partial selection keeps the cost at
        // O(n + k log k) instead of a full sort.
        op = match resolved_limit {
            Some(limit) => {
                let keep = resolved_offset.unwrap_or(0).saturating_add(limit) as usize;
                Box::new(Sort::with_keep(op, keys, params.to_vec(), keep))
            }
            None => Box::new(Sort::new(op, keys, params.to_vec())),
        };
    }

    if resolved_offset.is_some() || resolved_limit.is_some() {
        op = Box::new(Limit::new(op, resolved_offset.unwrap_or(0), resolved_limit));
    }

    let (_cols, mut rows) = run_to_rows(op.as_mut()).map_err(|e| match e {
        uqa_execution::physical::ExecError::SQL(err) => err,
        uqa_execution::physical::ExecError::Other(msg) => SQLError::Internal(msg),
    })?;
    if !stmt.order_by.is_empty() {
        for row in &mut rows {
            row.retain(|key, _| !key.starts_with(ORDER_KEY_PREFIX));
        }
    }
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
    let doc_ids: Vec<DocId> = scored.iter().map(|entry| entry.doc_id).collect();
    let documents = engine.get_documents_bulk(table, &doc_ids);
    let mut rows = Vec::with_capacity(scored.len());
    for entry in scored {
        let mut document = documents.get(&entry.doc_id).cloned().unwrap_or_default();
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
        if let Some(value) = projected_value_from_row(&proj.expr, document) {
            row.insert(label, value);
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
