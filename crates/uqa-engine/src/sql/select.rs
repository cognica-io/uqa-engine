//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL SELECT, set-operation, `CtePlan`, ordering, and projection execution.

use std::collections::HashSet;

use uqa_core::DocId;
use uqa_execution::{eval_scalar, ScalarEvalContext, ScalarExpr, ScalarFrameBound};
use uqa_joins::row_join::JoinKey;
use uqa_planner::{
    AccessPathPlan, ComputePlan, CtePlan, ProjectionPlan, QueryBlockPlan, QueryPlan,
    RelationalPlan, SourcePlan, UnifiedPlan,
};

use super::scalar::{eval_physical_scalar, PhysicalEvalContext, PhysicalSubqueryRunner};
use super::{
    aggregate_join_rows, build_aggregate_rows, build_join_rows_with_ctes,
    build_join_rows_with_ctes_filtered, build_join_rows_with_ctes_filtered_by_qualifier,
    build_join_rows_with_ctes_filtered_filtered_by_qualifier,
    build_join_rows_with_ctes_filtered_pruned,
    build_join_rows_with_ctes_filtered_pruned_filtered_by_qualifier,
    build_join_rows_with_ctes_pruned, build_join_rows_with_ctes_pruned_filtered_by_qualifier,
    compute_window_columns, engine_func_intercept, execute_function, execute_function_with_top_k,
    execute_lateral_subquery, execute_mixed_where, expect_column_name, has_aggregate, has_window,
    project_join_row_with_plan, projected_value_from_row, projection_label_at, BTreeMap, BTreeSet,
    BinaryOp, ColumnPrune, Document, Engine, QualifierFilters, ResultRow, SQLError, SQLParam,
    SQLResult, ScoredEntry, SetOpKind, Value, DOC_ID_COLUMN, MERGE_ACTION_COLUMN, SCORE_COLUMN,
};

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

/// Execute the physical relational plan directly. CTEs, set-operation
/// branches, values, and query blocks recurse through plan children; query
/// blocks select physical access and row operators without reconstructing a
/// parser statement.
pub(super) fn execute_query_plan(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut ctes = CteScope::new();
    execute_query_plan_with_ctes(engine, plan, params, &mut ctes)
}

/// Execute a physical query plan while preserving the caller's CTE scope.
pub(super) fn execute_query_plan_with_ctes(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    if !plan.ctes.is_empty() {
        let filters = cte_output_filters(plan);
        materialize_plan_ctes_with_filters(engine, &plan.ctes, params, ctes, &filters)?;
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            execute_query_block_plan(engine, block, params, ctes, None)
        }
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
        } => {
            let lhs = execute_query_plan_with_ctes(engine, left, params, ctes)?;
            let rhs = execute_query_plan_with_ctes(engine, right, params, ctes)?;
            let mut combined = combine_set_results(*kind, *all, lhs, rhs);
            if !order_by.is_empty() || limit.is_some() || offset.is_some() {
                let synthetic = QueryBlockPlan {
                    projections: Vec::new(),
                    from: None,
                    r#where: None,
                    compute: ComputePlan::Project,
                    group_by: Vec::new(),
                    grouping_sets: Vec::new(),
                    having: None,
                    order_by: order_by.clone(),
                    limit: limit.as_deref().cloned(),
                    offset: offset.as_deref().cloned(),
                    distinct: false,
                    distinct_on: Vec::new(),
                    subqueries: subqueries.clone(),
                    access: AccessPathPlan::Row,
                };
                let columns = combined.columns.clone();
                let mut ordering_scope = ctes.clone();
                ordering_scope.scalar_subqueries.clone_from(subqueries);
                combined.rows = apply_row_order_limit_with_ctes(
                    combined.rows,
                    &synthetic,
                    engine,
                    params,
                    &ordering_scope,
                )?;
                combined.columns = columns;
            }
            Ok(combined)
        }
        RelationalPlan::Values { rows, subqueries } => {
            execute_plan_values(engine, rows, subqueries, params, ctes)
        }
    }
}

fn execute_query_block_plan(
    engine: &Engine,
    block: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    prepared_exists_filter: Option<&ExistsMembershipPlan>,
) -> Result<SQLResult, SQLError> {
    let saved_subqueries = std::mem::replace(&mut ctes.scalar_subqueries, block.subqueries.clone());
    let result = (|| {
        let defer_distinct_limit = should_defer_distinct_limit(block);
        let execution = select_execution_stmt(block, defer_distinct_limit);
        let mut result = run_query_block_with_prepared_exists(
            engine,
            block,
            &execution,
            params,
            ctes,
            prepared_exists_filter,
        )?;
        result = apply_select_distinct(engine, block, result, params, ctes)?;
        if defer_distinct_limit {
            let columns = result.columns.clone();
            result.rows = apply_limit_offset_only(result.rows, block, engine, params, ctes)?;
            result.columns = columns;
        }
        Ok(result)
    })();
    ctes.scalar_subqueries = saved_subqueries;
    result
}

pub(super) fn combine_set_results(
    kind: SetOpKind,
    all: bool,
    lhs: SQLResult,
    rhs: SQLResult,
) -> SQLResult {
    match (kind, all) {
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
                .filter(|row| rhs.rows.iter().any(|candidate| candidate == row))
                .collect();
            if !all {
                rows = distinct_rows_stable(rows);
            }
            SQLResult::from_rows(lhs.columns, rows)
        }
        (SetOpKind::Except, _) => {
            let mut rows: Vec<ResultRow> = lhs
                .rows
                .into_iter()
                .filter(|row| !rhs.rows.iter().any(|candidate| candidate == row))
                .collect();
            if !all {
                rows = distinct_rows_stable(rows);
            }
            SQLResult::from_rows(lhs.columns, rows)
        }
    }
}

fn execute_plan_values(
    engine: &Engine,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    if rows.is_empty() {
        return Ok(SQLResult::empty());
    }
    let columns: Vec<String> = (0..rows[0].len())
        .map(|index| format!("column{}", index + 1))
        .collect();
    let hook = ScopedEngineHook::new(engine, ctes);
    let context = PhysicalEvalContext::new(None, params)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    let mut output = Vec::with_capacity(rows.len());
    for source in rows {
        if source.len() != columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "VALUES row width {} does not match first row width {}",
                source.len(),
                columns.len()
            )));
        }
        let mut row = ResultRow::new();
        for (index, expression) in source.iter().enumerate() {
            row.insert(
                columns[index].clone(),
                eval_physical_scalar(expression, subqueries, &context)?,
            );
        }
        output.push(row);
    }
    Ok(SQLResult::from_rows(columns, output))
}

pub(super) fn materialize_plan_ctes(
    engine: &Engine,
    plans: &[CtePlan],
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<(), SQLError> {
    materialize_plan_ctes_with_filters(engine, plans, params, ctes, &BTreeMap::new())
}

fn materialize_plan_ctes_with_filters(
    engine: &Engine,
    plans: &[CtePlan],
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_filters: &BTreeMap<String, (String, ScalarExpr)>,
) -> Result<(), SQLError> {
    for plan in plans {
        let rows = if plan.recursive {
            materialize_recursive_cte(engine, plan, params, ctes, output_filters.get(&plan.name))?
        } else {
            let result = execute_query_plan_with_ctes(engine, &plan.query, params, ctes)?;
            apply_cte_column_aliases(result.rows, &result.columns, &plan.columns)
        };
        ctes.insert_materialized(plan.name.clone(), rows);
    }
    Ok(())
}

/// Render the inner statement as an EXPLAIN-style plan result. Mirrors
/// the canonical UQA implementation's `_explain_plan`: returns a single-column `plan` table with
/// one row per line.
pub(super) fn run_explain(
    _engine: &Engine,
    body: &UnifiedPlan,
    _params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let plan_text = match body {
        UnifiedPlan::Query(query) => format_query_plan(query),
        UnifiedPlan::Command(command) => format!("{}\n  {command:#?}", command.name()),
    };
    let mut rows: Vec<ResultRow> = Vec::new();
    for line in plan_text.split('\n') {
        let mut r = ResultRow::new();
        r.insert("plan".to_string(), Value::Str(line.to_string()));
        rows.push(r);
    }
    Ok(SQLResult {
        columns: vec!["plan".to_string()],
        rows,
        affected_rows: 0,
    })
}

fn format_query_plan(plan: &QueryPlan) -> String {
    match &plan.root {
        RelationalPlan::QueryBlock(block) => format_select_plan(block),
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => format!(
            "SetOp\n  kind={kind:?}\n  all={all}\n  left=({})\n  right=({})\n  order_by={}\n  limit={}\n  offset={}",
            format_query_plan(left).replace('\n', "\n    "),
            format_query_plan(right).replace('\n', "\n    "),
            order_by.len(),
            limit
                .as_deref()
                .map_or_else(|| "none".into(), explain_int_expr),
            offset
                .as_deref()
                .map_or_else(|| "none".into(), explain_int_expr),
        ),
        RelationalPlan::Values { rows, .. } => format!("Values\n  rows={}", rows.len()),
    }
}

fn format_select_plan(stmt: &QueryBlockPlan) -> String {
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

fn should_defer_distinct_limit(stmt: &QueryBlockPlan) -> bool {
    stmt.distinct && (stmt.limit.is_some() || stmt.offset.is_some())
}

fn select_execution_stmt(stmt: &QueryBlockPlan, defer_distinct_limit: bool) -> QueryBlockPlan {
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
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    let row = ResultRow::new();
    let columns = projection_columns(&stmt.projections);
    let hook = ScopedEngineHook::new(engine, ctes);
    // `SELECT 1 WHERE false` must produce zero rows: the WHERE clause
    // applies even without a FROM (three-valued: NULL filters too).
    if let Some(filter) = stmt.r#where.as_ref() {
        let ctx = PhysicalEvalContext::new(Some(&row), params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        let keep = eval_physical_scalar(filter, &ctes.scalar_subqueries, &ctx)?;
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
    if let Some(result) = expand_projection_srf(engine, &hook, stmt, &row, params)? {
        return Ok(result);
    }
    let projected = super::from_rows::project_join_row_with_plan(
        engine,
        &hook,
        &hook,
        &ctes.scalar_subqueries,
        &row,
        &stmt.projections,
        params,
    )?;
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
    hook: &ScopedEngineHook<'_>,
    stmt: &QueryBlockPlan,
    row: &ResultRow,
    params: &[SQLParam],
) -> Result<Option<SQLResult>, SQLError> {
    if stmt.projections.len() != 1 {
        return Ok(None);
    }
    let projection = &stmt.projections[0];
    let ScalarExpr::Func { name, args, .. } = &projection.expr else {
        return Ok(None);
    };
    let lower = name.to_ascii_lowercase();
    let columns = projection_columns(&stmt.projections);
    let label = &columns[0];
    // Object-key extractors return a set of rows in PostgreSQL; the
    // scalar evaluator produces the key list, unpacked here.
    if matches!(lower.as_str(), "json_object_keys" | "jsonb_object_keys") {
        let ctx = PhysicalEvalContext::new(Some(row), params)
            .with_function_hook(hook)
            .with_subquery_runner(hook);
        let value = eval_physical_scalar(&projection.expr, &stmt.subqueries, &ctx)?;
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
    let context = super::from_rows::TableFunctionEvalContext::new(
        engine,
        params,
        hook,
        hook,
        &stmt.subqueries,
    );
    let produced =
        super::from_rows::build_table_function_rows(&context, &lower, args, None, &[], &[])?;
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
    stmt: &QueryBlockPlan,
    mut result: SQLResult,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    if stmt.distinct {
        result.rows = if stmt.distinct_on.is_empty() {
            distinct_rows_stable(result.rows)
        } else {
            distinct_on_rows(engine, result.rows, &stmt.distinct_on, params, ctes)?
        };
    }
    Ok(result)
}

fn apply_limit_offset_only(
    rows: Vec<ResultRow>,
    stmt: &QueryBlockPlan,
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    let synthetic = QueryBlockPlan {
        projections: Vec::new(),
        from: None,
        r#where: None,
        compute: ComputePlan::Project,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        having: None,
        order_by: Vec::new(),
        limit: stmt.limit.clone(),
        offset: stmt.offset.clone(),
        distinct: false,
        distinct_on: Vec::new(),
        subqueries: stmt.subqueries.clone(),
        access: AccessPathPlan::Row,
    };
    apply_row_order_limit_with_ctes(rows, &synthetic, engine, params, ctes)
}

fn distinct_on_rows(
    engine: &Engine,
    rows: Vec<ResultRow>,
    keys: &[ScalarExpr],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(rows.len());
    let mut out = Vec::with_capacity(rows.len());
    let hook = ScopedEngineHook::new(engine, ctes);
    for row in rows {
        let ctx = PhysicalEvalContext::new(Some(&row), params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        let mut key = String::new();
        for expr in keys {
            let value = eval_physical_scalar(expr, &ctes.scalar_subqueries, &ctx)?;
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

#[derive(Clone, Default)]
pub(crate) struct CteScope {
    pub(super) rows: BTreeMap<String, Vec<ResultRow>>,
    pub(super) scalar_subqueries: Vec<QueryPlan>,
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
}

impl<'a> ScopedEngineHook<'a> {
    pub(super) fn new(engine: &'a Engine, ctes: &'a CteScope) -> Self {
        Self { engine, ctes }
    }
}

struct ExistsMembershipPlan {
    filters: Vec<ExistsMembershipFilter>,
    residual: Option<ScalarExpr>,
}

struct ExistsMembershipFilter {
    outer_exprs: Vec<ScalarExpr>,
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
}

impl PhysicalSubqueryRunner for ScopedEngineHook<'_> {
    fn execute_subquery(
        &self,
        plan: &QueryPlan,
        outer_row: Option<&ResultRow>,
        params: &[SQLParam],
    ) -> Result<SQLResult, SQLError> {
        if let Some(outer_row) = outer_row {
            return execute_lateral_query_plan(self.engine, plan, outer_row, params, self.ctes);
        }
        let mut scoped_ctes = self.ctes.clone();
        execute_query_plan_with_ctes(self.engine, plan, params, &mut scoped_ctes)
    }
}

fn execute_lateral_query_plan(
    engine: &Engine,
    plan: &QueryPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    execute_lateral_subquery(engine, plan, outer_row, params, ctes)
}

fn collect_local_qualifiers(from: &SourcePlan, out: &mut std::collections::BTreeSet<String>) {
    match from {
        SourcePlan::Table { name, alias } => {
            out.insert(name.clone());
            if let Some(alias) = alias {
                out.insert(alias.clone());
            }
        }
        SourcePlan::Join { left, right, .. } => {
            collect_local_qualifiers(left, out);
            collect_local_qualifiers(right, out);
        }
        SourcePlan::Values { alias, .. }
        | SourcePlan::Function { alias, .. }
        | SourcePlan::Subquery { alias, .. } => {
            if let Some(alias) = alias {
                out.insert(alias.clone());
            }
        }
    }
}

fn prepare_exists_membership_filter(
    engine: &Engine,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<Option<ExistsMembershipPlan>, SQLError> {
    match filter {
        _ if exists_predicate_parts(filter, &ctes.scalar_subqueries).is_some() => {
            let Some(filter) = prepare_single_exists_membership(engine, filter, params, ctes)?
            else {
                return Ok(None);
            };
            Ok(Some(ExistsMembershipPlan {
                filters: vec![filter],
                residual: None,
            }))
        }
        ScalarExpr::And(items) => {
            let mut filters = Vec::new();
            let mut residual = Vec::new();
            for item in items {
                if exists_predicate_parts(item, &ctes.scalar_subqueries).is_some() {
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

fn prepare_single_exists_membership(
    engine: &Engine,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<Option<ExistsMembershipFilter>, SQLError> {
    let Some((body, negated)) = exists_predicate_parts(filter, &ctes.scalar_subqueries) else {
        return Ok(None);
    };
    let body = body.clone();
    if body.distinct
        || !body.distinct_on.is_empty()
        || !body.group_by.is_empty()
        || !body.grouping_sets.is_empty()
        || body.having.is_some()
        || !body.order_by.is_empty()
        || body.limit.is_some()
        || body.offset.is_some()
        || select_contains_volatile_function(&body)
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
            let hook = ScopedEngineHook::new(engine, ctes);
            let ctx = PhysicalEvalContext::new(Some(row), params)
                .with_function_hook(&hook)
                .with_subquery_runner(&hook);
            if !uqa_sql::expr::truthy(&eval_physical_scalar(local_filter, &body.subqueries, &ctx)?)
            {
                continue;
            }
        }
        let hook = ScopedEngineHook::new(engine, ctes);
        let ctx = PhysicalEvalContext::new(Some(row), params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        if let Some(key) = membership_key_for_exprs(&inner_exprs, &body.subqueries, &ctx)? {
            inner_keys.insert(key);
        }
    }

    Ok(Some(ExistsMembershipFilter {
        outer_exprs,
        inner_keys,
        negated,
    }))
}

fn exists_predicate_parts<'a>(
    expr: &ScalarExpr,
    subqueries: &'a [QueryPlan],
) -> Option<(&'a QueryBlockPlan, bool)> {
    let (slot, negated) = match expr {
        ScalarExpr::Exists { subquery, negated } => (*subquery, *negated),
        ScalarExpr::Not(inner) => match inner.as_ref() {
            ScalarExpr::Exists { subquery, negated } => (*subquery, !*negated),
            _ => return None,
        },
        _ => return None,
    };
    let query = subqueries.get(slot)?;
    if !query.ctes.is_empty() {
        return None;
    }
    match &query.root {
        RelationalPlan::QueryBlock(block) => Some((block, negated)),
        RelationalPlan::SetOp { .. } | RelationalPlan::Values { .. } => None,
    }
}

fn split_exists_membership_where(
    expr: &ScalarExpr,
    local_qualifiers: &std::collections::BTreeSet<String>,
) -> Option<(Vec<ScalarExpr>, Vec<ScalarExpr>, Option<ScalarExpr>)> {
    match expr {
        ScalarExpr::And(items) => {
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

fn combine_and_items(items: Vec<ScalarExpr>) -> Option<ScalarExpr> {
    match items.len() {
        0 => None,
        1 => items.into_iter().next(),
        _ => Some(ScalarExpr::And(items)),
    }
}

fn split_correlated_equality(
    expr: &ScalarExpr,
    local_qualifiers: &std::collections::BTreeSet<String>,
) -> Option<(ScalarExpr, ScalarExpr)> {
    let ScalarExpr::Binary {
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
    expr: &ScalarExpr,
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
    ctes: &CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::with_capacity(rows.len());
    let hook = ScopedEngineHook::new(engine, ctes);
    for row in rows {
        let ctx = PhysicalEvalContext::new(Some(&row), params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        if let Some(residual) = plan.residual.as_ref() {
            if !uqa_sql::expr::truthy(&eval_physical_scalar(
                residual,
                &ctes.scalar_subqueries,
                &ctx,
            )?) {
                continue;
            }
        }
        let mut keep = true;
        for filter in &plan.filters {
            let contains =
                membership_key_for_exprs(&filter.outer_exprs, &ctes.scalar_subqueries, &ctx)?
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

fn expr_applicable_to_row(expr: &ScalarExpr, row: &ResultRow) -> bool {
    match expr {
        ScalarExpr::Column(name) => column_present(row, name),
        ScalarExpr::QualifiedColumn {
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
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => true,
        ScalarExpr::Func {
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
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().all(|expr| expr_applicable_to_row(expr, row))
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_applicable_to_row(lhs, row) && expr_applicable_to_row(rhs, row)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_applicable_to_row(inner, row),
        ScalarExpr::Between { expr, low, high } => {
            expr_applicable_to_row(expr, row)
                && expr_applicable_to_row(low, row)
                && expr_applicable_to_row(high, row)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_applicable_to_row(expr, row)
                && list.iter().all(|item| expr_applicable_to_row(item, row))
        }
        ScalarExpr::Case {
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
        ScalarExpr::Star
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
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
    exprs: &[ScalarExpr],
    subqueries: &[QueryPlan],
    ctx: &PhysicalEvalContext<'_>,
) -> Result<Option<Vec<JoinKey>>, SQLError> {
    let mut key = Vec::with_capacity(exprs.len());
    for expr in exprs {
        let value = eval_physical_scalar(expr, subqueries, ctx)?;
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        key.push(JoinKey::new(&value));
    }
    Ok(Some(key))
}

fn expr_references_outer(
    expr: &ScalarExpr,
    local_qualifiers: &std::collections::BTreeSet<String>,
    has_local_from: bool,
) -> bool {
    match expr {
        ScalarExpr::Star | ScalarExpr::Literal(_) | ScalarExpr::Param(_) => false,
        ScalarExpr::Column(_) => !has_local_from,
        ScalarExpr::QualifiedColumn { qualifier, .. } => !local_qualifiers.contains(qualifier),
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => items
            .iter()
            .any(|item| expr_references_outer(item, local_qualifiers, has_local_from)),
        ScalarExpr::Func {
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
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_references_outer(lhs, local_qualifiers, has_local_from)
                || expr_references_outer(rhs, local_qualifiers, has_local_from)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            expr_references_outer(inner, local_qualifiers, has_local_from)
        }
        ScalarExpr::Between { expr, low, high } => {
            expr_references_outer(expr, local_qualifiers, has_local_from)
                || expr_references_outer(low, local_qualifiers, has_local_from)
                || expr_references_outer(high, local_qualifiers, has_local_from)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_references_outer(expr, local_qualifiers, has_local_from)
                || list
                    .iter()
                    .any(|item| expr_references_outer(item, local_qualifiers, has_local_from))
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
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
        ScalarExpr::Case {
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
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => true,
    }
}

fn expr_contains_subquery(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => true,
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_contains_subquery)
        }
        ScalarExpr::Func {
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
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_contains_subquery(lhs) || expr_contains_subquery(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_contains_subquery(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_contains_subquery(expr)
                || expr_contains_subquery(low)
                || expr_contains_subquery(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_contains_subquery(expr) || list.iter().any(expr_contains_subquery)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
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
        ScalarExpr::Case {
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
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_) => false,
    }
}

pub(super) fn select_contains_volatile_function(stmt: &QueryBlockPlan) -> bool {
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
}

fn expr_contains_volatile_function(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Func {
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
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_contains_volatile_function)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_contains_volatile_function(lhs) || expr_contains_volatile_function(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_contains_volatile_function(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_contains_volatile_function(expr)
                || expr_contains_volatile_function(low)
                || expr_contains_volatile_function(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_contains_volatile_function(expr)
                || list.iter().any(expr_contains_volatile_function)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
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
        ScalarExpr::Case {
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
        // Query-valued children are inspected by the enclosing `QueryPlan`.
        // At expression-only optimization sites, treating them as volatile is
        // the safe choice because it prevents duplication or reordering.
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => true,
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_) => false,
    }
}

fn frame_bound_contains_volatile_function(bound: &ScalarFrameBound) -> bool {
    match bound {
        ScalarFrameBound::Preceding(expr) | ScalarFrameBound::Following(expr) => {
            expr_contains_volatile_function(expr)
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => false,
    }
}

fn frame_bound_contains_subquery(bound: &ScalarFrameBound) -> bool {
    match bound {
        ScalarFrameBound::Preceding(expr) | ScalarFrameBound::Following(expr) => {
            expr_contains_subquery(expr)
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => false,
    }
}

fn frame_bound_references_outer(
    bound: &ScalarFrameBound,
    local_qualifiers: &std::collections::BTreeSet<String>,
    has_local_from: bool,
) -> bool {
    match bound {
        ScalarFrameBound::Preceding(expr) | ScalarFrameBound::Following(expr) => {
            expr_references_outer(expr, local_qualifiers, has_local_from)
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => false,
    }
}

fn run_query_block_with_prepared_exists(
    engine: &Engine,
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    prepared_exists_filter: Option<&ExistsMembershipPlan>,
) -> Result<SQLResult, SQLError> {
    let Some(from) = stmt.from.as_ref() else {
        return run_select_without_from(engine, stmt, params, ctes);
    };

    // Set-op branches, CTEs, and derived-table bodies still need the same
    // search-aware single-table physical access path as top-level queries;
    // otherwise registry-backed predicates such as
    // `fuse_log_odds(bayesian_match(...), knn_match(...))` fall
    // through to scalar expression evaluation.
    if prepared_exists_filter.is_none() {
        if let SourcePlan::Table { name, alias } = from {
            if alias.is_none() && engine.foreign_table(name).is_some() {
                return run_single_foreign_select(engine, name, block, stmt, params, ctes);
            }
            let is_virtual = name.contains('.')
                || (engine.table(name).is_none() && engine.foreign_table(name).is_none());
            let has_subquery_filter = stmt.r#where.as_ref().is_some_and(expr_contains_subquery);
            let has_subquery_projection = stmt
                .projections
                .iter()
                .any(|projection| expr_contains_subquery(&projection.expr));
            if alias.is_none()
                && !matches!(&block.compute, ComputePlan::Window)
                && !is_virtual
                && !has_subquery_filter
                && !has_subquery_projection
            {
                return run_single_table_select(engine, name, block, stmt, params, ctes);
            }
        }
    }

    if let Some(filter) = stmt.r#where.as_ref() {
        super::validate_joined_expr_text_match_fields(engine, from, filter)?;
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
        let exists_eval_scope = ctes.clone();
        let mut row_filter = |rows: &mut Vec<ResultRow>| -> Result<(), SQLError> {
            if !early_exists_applied
                && exists_membership_plan_applicable_to_rows(exists_filter, rows)
            {
                let filtered = apply_exists_membership_filter(
                    engine,
                    std::mem::take(rows),
                    exists_filter,
                    params,
                    &exists_eval_scope,
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

    // Aggregate and window compute plans use their stateful physical
    // operators. Pure projection plans flow through the row pipeline, with
    // source-namespace ordering/limiting before the final Project operator.
    let final_filter =
        final_filter_after_qualifier_pushdown(stmt, from, qualifier_filters.as_ref());
    let filtered = if let Some(filter) = final_filter.as_ref() {
        if joined.is_empty() {
            joined
        } else if let Some(exists_filter) = prepared_exists_filter {
            if early_exists_applied {
                joined
            } else {
                apply_exists_membership_filter(engine, joined, exists_filter, params, ctes)?
            }
        } else if let Some(exists_filter) = owned_exists_filter.as_ref() {
            if early_exists_applied {
                joined
            } else {
                apply_exists_membership_filter(engine, joined, exists_filter, params, ctes)?
            }
        } else {
            let scoped_hook = ScopedEngineHook::new(engine, ctes);
            let mut out: Vec<ResultRow> = Vec::with_capacity(joined.len());
            for row in joined {
                let ctx = PhysicalEvalContext::new(Some(&row), params)
                    .with_function_hook(&scoped_hook)
                    .with_subquery_runner(&scoped_hook);
                if uqa_sql::expr::truthy(&eval_physical_scalar(
                    filter,
                    &ctes.scalar_subqueries,
                    &ctx,
                )?) {
                    out.push(row);
                }
            }
            out
        }
    } else {
        joined
    };

    if matches!(&block.compute, ComputePlan::Aggregate) {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &filtered, params, ctes)?;
        let rows = apply_row_order_limit_with_ctes(rows, stmt, engine, params, ctes)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    if matches!(&block.compute, ComputePlan::Window) {
        let columns = projection_columns(&stmt.projections);
        let windowed = compute_window_columns(engine, &stmt.projections, filtered, params, ctes)?;
        let scoped_hook = ScopedEngineHook::new(engine, ctes);
        let mut rows: Vec<ResultRow> = windowed
            .rows
            .iter()
            .map(|src| {
                project_join_row_with_plan(
                    engine,
                    &scoped_hook,
                    &scoped_hook,
                    &ctes.scalar_subqueries,
                    src,
                    &windowed.projections,
                    params,
                )
            })
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit_with_ctes(rows, stmt, engine, params, ctes)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    // Pure projection: stage source rows, then run the physical Project.
    let projected = volcano_project_sort_limit(engine, &filtered, stmt, params, ctes)?;
    let columns = expand_from_star_columns(
        engine,
        projection_columns(&stmt.projections),
        &stmt.projections,
        from,
    );
    Ok(SQLResult::from_rows(columns, projected))
}

fn column_prune_for_stmt(stmt: &QueryBlockPlan, from: &SourcePlan) -> Option<ColumnPrune> {
    if has_window(&stmt.projections)
        || stmt.projections.iter().any(|projection| {
            matches!(projection.expr, ScalarExpr::Star)
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

fn collect_from_qualifiers(from: &SourcePlan, out: &mut Vec<String>) {
    match from {
        SourcePlan::Table { name, alias } => {
            out.push(alias.clone().unwrap_or_else(|| name.clone()));
        }
        SourcePlan::Join { left, right, .. } => {
            collect_from_qualifiers(left, out);
            collect_from_qualifiers(right, out);
        }
        SourcePlan::Values { alias, .. }
        | SourcePlan::Function { alias, .. }
        | SourcePlan::Subquery { alias, .. } => {
            if let Some(alias) = alias {
                out.push(alias.clone());
            }
        }
    }
}

fn collect_from_prune_columns(
    from: &SourcePlan,
    qualifiers: &[String],
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    match from {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            collect_from_prune_columns(left, qualifiers, prune, valid);
            collect_from_prune_columns(right, qualifiers, prune, valid);
            if let Some(on) = on.as_ref() {
                collect_expr_prune_columns(on, qualifiers, prune, valid);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for row in rows {
                for expr in row {
                    collect_expr_prune_columns(expr, qualifiers, prune, valid);
                }
            }
        }
        SourcePlan::Function { args, .. } => {
            for expr in args {
                collect_expr_prune_columns(expr, qualifiers, prune, valid);
            }
        }
        SourcePlan::Subquery { .. } => {
            *valid = false;
        }
        SourcePlan::Table { .. } => {}
    }
}

fn collect_expr_prune_columns(
    expr: &ScalarExpr,
    qualifiers: &[String],
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    match expr {
        ScalarExpr::Column(column) => {
            for qualifier in qualifiers {
                if let Some(columns) = prune.get_mut(qualifier) {
                    columns.insert(column.clone());
                }
            }
        }
        ScalarExpr::QualifiedColumn {
            qualifier, column, ..
        } => {
            if let Some(columns) = prune.get_mut(qualifier) {
                columns.insert(column.clone());
            } else {
                *valid = false;
            }
        }
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => {}
        ScalarExpr::Star
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => {
            *valid = false;
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                collect_expr_prune_columns(item, qualifiers, prune, valid);
            }
        }
        ScalarExpr::Func {
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
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_expr_prune_columns(lhs, qualifiers, prune, valid);
            collect_expr_prune_columns(rhs, qualifiers, prune, valid);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            collect_expr_prune_columns(inner, qualifiers, prune, valid);
        }
        ScalarExpr::Between { expr, low, high } => {
            collect_expr_prune_columns(expr, qualifiers, prune, valid);
            collect_expr_prune_columns(low, qualifiers, prune, valid);
            collect_expr_prune_columns(high, qualifiers, prune, valid);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_expr_prune_columns(expr, qualifiers, prune, valid);
            for item in list {
                collect_expr_prune_columns(item, qualifiers, prune, valid);
            }
        }
        ScalarExpr::WindowCall { .. } => {
            *valid = false;
        }
        ScalarExpr::Case {
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

fn qualifier_filters_for_stmt(
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
) -> Option<QualifierFilters> {
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
    part: &ScalarExpr,
    from_quals: &BTreeSet<String>,
    single_qualifier: Option<&str>,
) -> Option<(String, ScalarExpr)> {
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
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filters: Option<&QualifierFilters>,
) -> Option<ScalarExpr> {
    let filter = stmt.r#where.as_ref()?;
    if filters.is_none() || !qualifier_filter_elision_safe(from) {
        return Some(filter.clone());
    }
    let from_quals = from_qualifier_set(from);
    let single_qualifier = (from_quals.len() == 1)
        .then(|| from_quals.iter().next().cloned())
        .flatten();
    let residual: Vec<ScalarExpr> = flatten_and_filter_parts(filter)
        .into_iter()
        .filter(|part| {
            qualifier_filter_for_part(part, &from_quals, single_qualifier.as_deref()).is_none()
        })
        .cloned()
        .collect();
    combine_filter_parts(residual)
}

fn qualifier_filter_elision_safe(from: &SourcePlan) -> bool {
    match from {
        SourcePlan::Join {
            left, right, kind, ..
        } => {
            matches!(
                kind,
                uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross
            ) && qualifier_filter_elision_safe(left)
                && qualifier_filter_elision_safe(right)
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::Subquery { .. } => true,
    }
}

fn combine_filter_parts(mut parts: Vec<ScalarExpr>) -> Option<ScalarExpr> {
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(ScalarExpr::And(parts)),
    }
}

/// Find predicates on a directly referenced CTE output. The predicate remains
/// on the consumer and is duplicated into the CTE only when that CTE has one
/// reference in this query block. This makes the rewrite semantics-preserving
/// for shared CTE materializations.
fn cte_output_filters(plan: &QueryPlan) -> BTreeMap<String, (String, ScalarExpr)> {
    let RelationalPlan::QueryBlock(block) = &plan.root else {
        return BTreeMap::new();
    };
    let (Some(from), Some(filter)) = (block.from.as_ref(), block.r#where.as_ref()) else {
        return BTreeMap::new();
    };
    if expr_contains_subquery(filter) || expr_contains_volatile_function(filter) {
        return BTreeMap::new();
    }

    let cte_names: BTreeSet<&str> = plan.ctes.iter().map(|cte| cte.name.as_str()).collect();
    let mut references: BTreeMap<String, Vec<String>> = BTreeMap::new();
    collect_cte_source_references(from, &cte_names, &mut references);
    let qualifier_to_cte: BTreeMap<String, String> = references
        .into_iter()
        .filter_map(|(cte, qualifiers)| {
            (qualifiers.len() == 1).then(|| (qualifiers[0].clone(), cte))
        })
        .collect();
    if qualifier_to_cte.is_empty() {
        return BTreeMap::new();
    }

    let from_qualifiers = from_qualifier_set(from);
    let single_qualifier = (from_qualifiers.len() == 1)
        .then(|| from_qualifiers.iter().next().cloned())
        .flatten();
    let mut grouped: BTreeMap<String, (String, Vec<ScalarExpr>)> = BTreeMap::new();
    for part in flatten_and_filter_parts(filter) {
        let Some((qualifier, predicate)) =
            qualifier_filter_for_part(part, &from_qualifiers, single_qualifier.as_deref())
        else {
            continue;
        };
        let Some(cte_name) = qualifier_to_cte.get(&qualifier) else {
            continue;
        };
        let entry = grouped
            .entry(cte_name.clone())
            .or_insert_with(|| (qualifier, Vec::new()));
        entry.1.push(predicate);
    }

    grouped
        .into_iter()
        .filter_map(|(name, (qualifier, predicates))| {
            combine_filter_parts(predicates).map(|predicate| (name, (qualifier, predicate)))
        })
        .collect()
}

fn collect_cte_source_references(
    source: &SourcePlan,
    cte_names: &BTreeSet<&str>,
    references: &mut BTreeMap<String, Vec<String>>,
) {
    match source {
        SourcePlan::Table { name, alias } if cte_names.contains(name.as_str()) => {
            references
                .entry(name.clone())
                .or_default()
                .push(alias.clone().unwrap_or_else(|| name.clone()));
        }
        SourcePlan::Join { left, right, .. } => {
            collect_cte_source_references(left, cte_names, references);
            collect_cte_source_references(right, cte_names, references);
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::Subquery { .. } => {}
    }
}

/// Specialize a physical query plan with a predicate on its output columns.
/// The caller keeps the original predicate as a residual check; this function
/// only returns a plan when pushing the predicate below the output boundary is
/// provably safe.
pub(super) fn push_output_filter_into_query_plan(
    engine: &Engine,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<QueryPlan> {
    if expr_contains_subquery(filter) || expr_contains_volatile_function(filter) {
        return None;
    }
    let specialized =
        specialize_query_output_filter(plan, qualifier, filter, output_columns_override)?;
    let aggregate_classifier = |name: &str| engine.has_registered_aggregate_function(name);
    match uqa_planner::optimizer::optimize_with_aggregates(
        UnifiedPlan::Query(Box::new(specialized)),
        &uqa_planner::optimizer::OptimizerConfig::default(),
        &aggregate_classifier,
    ) {
        UnifiedPlan::Query(plan) => Some(*plan),
        UnifiedPlan::Command(_) => unreachable!("query optimizer changed the plan kind"),
    }
}

fn specialize_query_output_filter(
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<QueryPlan> {
    let mut specialized = plan.clone();
    specialize_relational_output_filter(
        &mut specialized.root,
        qualifier,
        filter,
        output_columns_override,
    )?;
    Some(specialized)
}

fn specialize_relational_output_filter(
    root: &mut RelationalPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<()> {
    match root {
        RelationalPlan::QueryBlock(block) => {
            specialize_query_block_output_filter(block, qualifier, filter, output_columns_override)
        }
        RelationalPlan::SetOp {
            left,
            right,
            limit,
            offset,
            ..
        } => {
            if limit.is_some() || offset.is_some() {
                return None;
            }
            let output_columns = match output_columns_override {
                Some(columns) => columns.to_vec(),
                None => query_plan_output_columns(left)?,
            };
            let specialized_left =
                specialize_query_output_filter(left, qualifier, filter, Some(&output_columns))?;
            let specialized_right =
                specialize_query_output_filter(right, qualifier, filter, Some(&output_columns))?;
            **left = specialized_left;
            **right = specialized_right;
            Some(())
        }
        RelationalPlan::Values { .. } => None,
    }
}

fn query_plan_output_columns(plan: &QueryPlan) -> Option<Vec<String>> {
    match &plan.root {
        RelationalPlan::QueryBlock(block) => Some(projection_columns(&block.projections)),
        RelationalPlan::SetOp { left, .. } => query_plan_output_columns(left),
        RelationalPlan::Values { rows, .. } => rows.first().map(|row| {
            (1..=row.len())
                .map(|index| format!("column{index}"))
                .collect()
        }),
    }
}

fn specialize_query_block_output_filter(
    block: &mut QueryBlockPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<()> {
    if block.limit.is_some()
        || block.offset.is_some()
        || matches!(block.compute, ComputePlan::Window)
        || !block.distinct_on.is_empty()
        || !block.grouping_sets.is_empty()
    {
        return None;
    }

    let output_columns = output_columns_override.map_or_else(
        || projection_columns(&block.projections),
        <[String]>::to_vec,
    );
    if output_columns.len() != block.projections.len() {
        return None;
    }
    let mut used = BTreeSet::new();
    let rewritten = rewrite_output_filter(
        filter,
        qualifier,
        &output_columns,
        &block.projections,
        &mut used,
    )?;
    if used.is_empty() {
        return None;
    }

    for index in &used {
        let expression = &block.projections[*index].expr;
        if matches!(expression, ScalarExpr::Star)
            || expression.contains_window()
            || expr_contains_subquery(expression)
            || expr_contains_volatile_function(expression)
        {
            return None;
        }
        if matches!(block.compute, ComputePlan::Aggregate)
            && !block.group_by.iter().any(|group| group == expression)
        {
            return None;
        }
    }
    if block.distinct
        && block
            .projections
            .iter()
            .enumerate()
            .any(|(index, projection)| {
                !used.contains(&index) && expr_contains_function(&projection.expr)
            })
    {
        return None;
    }

    block.r#where = match block.r#where.take() {
        Some(existing) => Some(ScalarExpr::And(vec![existing, rewritten])),
        None => Some(rewritten),
    };
    Some(())
}

fn rewrite_output_filter(
    expression: &ScalarExpr,
    qualifier: &str,
    output_columns: &[String],
    projections: &[ProjectionPlan],
    used: &mut BTreeSet<usize>,
) -> Option<ScalarExpr> {
    let map_column = |column: &str, used: &mut BTreeSet<usize>| {
        let index = output_columns
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(column))?;
        used.insert(index);
        Some(projections[index].expr.clone())
    };
    let recur = |expression: &ScalarExpr, used: &mut BTreeSet<usize>| {
        rewrite_output_filter(expression, qualifier, output_columns, projections, used)
    };

    Some(match expression {
        ScalarExpr::Column(column) => map_column(column, used)?,
        ScalarExpr::QualifiedColumn {
            qualifier: expression_qualifier,
            column,
            ..
        } if expression_qualifier.eq_ignore_ascii_case(qualifier) => map_column(column, used)?,
        ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Star
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => return None,
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => expression.clone(),
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
        ),
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| recur(arg, used))
                .collect::<Option<Vec<_>>>()?,
            distinct: *distinct,
            order_by: order_by
                .iter()
                .map(|order| {
                    Some(uqa_execution::ScalarOrder {
                        expr: recur(&order.expr, used)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            filter: match filter.as_deref() {
                Some(filter) => Some(Box::new(recur(filter, used)?)),
                None => None,
            },
        },
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(recur(lhs, used)?),
            rhs: Box::new(recur(rhs, used)?),
        },
        ScalarExpr::Not(inner) => ScalarExpr::Not(Box::new(recur(inner, used)?)),
        ScalarExpr::And(items) => ScalarExpr::And(
            items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
        ),
        ScalarExpr::Or(items) => ScalarExpr::Or(
            items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
        ),
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(recur(expr, used)?),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(recur(expr, used)?),
            low: Box::new(recur(low, used)?),
            high: Box::new(recur(high, used)?),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(recur(expr, used)?),
            list: list
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
            negated: *negated,
        },
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: match base.as_deref() {
                Some(base) => Some(Box::new(recur(base, used)?)),
                None => None,
            },
            when: when
                .iter()
                .map(|(condition, result)| Some((recur(condition, used)?, recur(result, used)?)))
                .collect::<Option<Vec<_>>>()?,
            else_branch: match else_branch.as_deref() {
                Some(branch) => Some(Box::new(recur(branch, used)?)),
                None => None,
            },
        },
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(recur(expr, used)?),
            ty: ty.clone(),
        },
    })
}

fn expr_contains_function(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Func { .. } | ScalarExpr::WindowCall { .. } => true,
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_contains_function)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_contains_function(lhs) || expr_contains_function(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_contains_function(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_contains_function(expr)
                || expr_contains_function(low)
                || expr_contains_function(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_contains_function(expr) || list.iter().any(expr_contains_function)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(expr_contains_function)
                || when.iter().any(|(condition, result)| {
                    expr_contains_function(condition) || expr_contains_function(result)
                })
                || else_branch.as_deref().is_some_and(expr_contains_function)
        }
        ScalarExpr::InSubquery { expr, .. } => expr_contains_function(expr),
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

fn flatten_and_filter_parts(expr: &ScalarExpr) -> Vec<&ScalarExpr> {
    match expr {
        ScalarExpr::And(items) => items.iter().flat_map(flatten_and_filter_parts).collect(),
        other => vec![other],
    }
}

fn from_qualifier_set(from: &SourcePlan) -> BTreeSet<String> {
    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    qualifiers.into_iter().collect()
}

fn expr_qualifiers(expr: &ScalarExpr) -> BTreeSet<String> {
    let mut qualifiers = BTreeSet::new();
    collect_expr_qualifiers(expr, &mut qualifiers);
    qualifiers
}

fn collect_expr_qualifiers(expr: &ScalarExpr, qualifiers: &mut BTreeSet<String>) {
    match expr {
        ScalarExpr::QualifiedColumn { qualifier, .. } => {
            qualifiers.insert(qualifier.clone());
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                collect_expr_qualifiers(item, qualifiers);
            }
        }
        ScalarExpr::Func {
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
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_expr_qualifiers(lhs, qualifiers);
            collect_expr_qualifiers(rhs, qualifiers);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            collect_expr_qualifiers(inner, qualifiers);
        }
        ScalarExpr::Between { expr, low, high } => {
            collect_expr_qualifiers(expr, qualifiers);
            collect_expr_qualifiers(low, qualifiers);
            collect_expr_qualifiers(high, qualifiers);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_expr_qualifiers(expr, qualifiers);
            for item in list {
                collect_expr_qualifiers(item, qualifiers);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
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
        ScalarExpr::Case {
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
        ScalarExpr::InSubquery { expr, .. } => collect_expr_qualifiers(expr, qualifiers),
        ScalarExpr::Column(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
}

fn expr_has_unqualified_column(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Column(_) => true,
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_has_unqualified_column)
        }
        ScalarExpr::Func {
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
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_has_unqualified_column(lhs) || expr_has_unqualified_column(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_has_unqualified_column(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_has_unqualified_column(expr)
                || expr_has_unqualified_column(low)
                || expr_has_unqualified_column(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_has_unqualified_column(expr) || list.iter().any(expr_has_unqualified_column)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(expr_has_unqualified_column)
                || spec.partition_by.iter().any(expr_has_unqualified_column)
                || spec
                    .order_by
                    .iter()
                    .any(|order| expr_has_unqualified_column(&order.expr))
        }
        ScalarExpr::Case {
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
        ScalarExpr::InSubquery { expr, .. } => expr_has_unqualified_column(expr),
        ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

fn qualify_unqualified_columns(expr: &ScalarExpr, qualifier: &str) -> ScalarExpr {
    match expr {
        ScalarExpr::Column(column) => ScalarExpr::qualified_column(qualifier, column),
        ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star => expr.clone(),
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::And(items) => ScalarExpr::And(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::Or(items) => ScalarExpr::Or(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(qualify_unqualified_columns(lhs, qualifier)),
            rhs: Box::new(qualify_unqualified_columns(rhs, qualifier)),
        },
        ScalarExpr::Not(inner) => {
            ScalarExpr::Not(Box::new(qualify_unqualified_columns(inner, qualifier)))
        }
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            low: Box::new(qualify_unqualified_columns(low, qualifier)),
            high: Box::new(qualify_unqualified_columns(high, qualifier)),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            list: list
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
            negated: *negated,
        },
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
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
        ScalarExpr::WindowCall { name, args, spec } => ScalarExpr::WindowCall {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| qualify_unqualified_columns(arg, qualifier))
                .collect(),
            spec: spec.clone(),
        },
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
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
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            ty: ty.clone(),
        },
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            subquery: *subquery,
            negated: *negated,
        },
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Exists { .. } => expr.clone(),
    }
}

fn expand_from_star_columns(
    engine: &Engine,
    columns: Vec<String>,
    projections: &[ProjectionPlan],
    from: &SourcePlan,
) -> Vec<String> {
    let has_star = projections
        .iter()
        .any(|p| matches!(p.expr, ScalarExpr::Star));
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

fn from_clause_output_columns(engine: &Engine, from: &SourcePlan) -> Vec<String> {
    match from {
        SourcePlan::Function {
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
        SourcePlan::Values {
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
        SourcePlan::Subquery {
            alias,
            column_aliases,
            ..
        } => qualify_output_columns(alias.as_deref(), column_aliases.clone()),
        SourcePlan::Join { left, right, .. } => {
            let mut cols = from_clause_output_columns(engine, left);
            cols.extend(from_clause_output_columns(engine, right));
            cols
        }
        SourcePlan::Table { .. } => Vec::new(),
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
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    let projection_hook = ScopedEngineHook::new(engine, ctes);
    // Some projection callsites (e.g. `text_match` in the SELECT
    // list) need the engine-side function registry, which the
    // execution-layer Project operator does not understand. Detect
    // those and fall back to the row-by-row engine projector so the
    // contract stays the same for SQL-function-bearing projections.
    let has_engine_funcs = stmt.projections.iter().any(|p| {
        let mut found = false;
        walk_expr(&p.expr, &mut |e| {
            if let ScalarExpr::Func { name, .. } = e {
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
        .any(|p| matches!(p.expr, ScalarExpr::Star));
    let has_subquery_projection = stmt
        .projections
        .iter()
        .any(|p| expr_contains_subquery(&p.expr));
    // Pre-projection ordering / limiting. PostgreSQL semantics allow
    // ORDER BY to reference columns that the SELECT list drops, so the
    // source rows must be staged before projection. This engine-side
    // stage uses the same physical scalar context as every other
    // relational site, including the query block's subquery arena.
    let resolved_offset =
        resolve_limit_offset_with_ctes(stmt.offset.as_ref(), engine, params, "OFFSET", ctes)?;
    let resolved_limit =
        resolve_limit_offset_with_ctes(stmt.limit.as_ref(), engine, params, "LIMIT", ctes)?;
    let top_k_rows = top_k_ordered_source_rows(
        engine,
        src_rows,
        stmt,
        params,
        resolved_offset,
        resolved_limit,
        ctes,
    )?;

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
            ctes,
        )?
    };

    if has_engine_funcs || has_star || has_subquery_projection {
        let rows: Vec<ResultRow> = staged
            .iter()
            .map(|src| {
                project_join_row_with_plan(
                    engine,
                    &projection_hook,
                    &projection_hook,
                    &stmt.subqueries,
                    src,
                    &stmt.projections,
                    params,
                )
            })
            .collect::<Result<_, _>>()?;
        return Ok(rows);
    }

    use uqa_execution::physical::{run_to_rows, ExecError, PhysicalOperator};
    use uqa_execution::relational::Project;
    use uqa_execution::scan::TableScan;

    let columns: Vec<String> = staged
        .first()
        .map(|r| r.keys().cloned().collect())
        .unwrap_or_default();
    let mut op: Box<dyn PhysicalOperator> = Box::new(TableScan::from_rows(columns, staged));

    let labels = projection_columns(&stmt.projections);
    let projections: Vec<(String, ScalarExpr)> = stmt
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
    stmt: &QueryBlockPlan,
    engine: &Engine,
    params: &[SQLParam],
    offset: Option<u64>,
    limit: Option<u64>,
    ctes: &CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    if stmt.order_by.is_empty() && offset.is_none() && limit.is_none() {
        return Ok(src_rows.to_vec());
    }
    if let Some(rows) =
        top_k_ordered_source_rows(engine, src_rows, stmt, params, offset, limit, ctes)?
    {
        return Ok(rows);
    }
    apply_row_order_limit_with_ctes(src_rows.to_vec(), stmt, engine, params, ctes)
}

fn top_k_ordered_source_rows(
    engine: &Engine,
    src_rows: &[ResultRow],
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    offset: Option<u64>,
    limit: Option<u64>,
    ctes: &CteScope,
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
    let hook = ScopedEngineHook::new(engine, ctes);
    for (idx, row) in src_rows.iter().enumerate() {
        let ctx = PhysicalEvalContext::new(Some(row), params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        let mut key_values = Vec::with_capacity(stmt.order_by.len());
        for order in &stmt.order_by {
            key_values.push(eval_physical_scalar(
                &order.expr,
                &ctes.scalar_subqueries,
                &ctx,
            )?);
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
    stmt: &QueryBlockPlan,
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

fn walk_expr<F: FnMut(&ScalarExpr)>(expr: &ScalarExpr, f: &mut F) {
    f(expr);
    match expr {
        ScalarExpr::And(parts) | ScalarExpr::Or(parts) => {
            for p in parts {
                walk_expr(p, f);
            }
        }
        ScalarExpr::Not(inner) => walk_expr(inner, f),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        ScalarExpr::IsNull { expr, .. } => walk_expr(expr, f),
        ScalarExpr::Between { expr, low, high } => {
            walk_expr(expr, f);
            walk_expr(low, f);
            walk_expr(high, f);
        }
        ScalarExpr::InList { expr, list, .. } => {
            walk_expr(expr, f);
            for p in list {
                walk_expr(p, f);
            }
        }
        ScalarExpr::Func { args, .. } | ScalarExpr::WindowCall { args, .. } => {
            for p in args {
                walk_expr(p, f);
            }
        }
        ScalarExpr::Case {
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
        ScalarExpr::Cast { expr, .. } => walk_expr(expr, f),
        ScalarExpr::Array(items) => {
            for p in items {
                walk_expr(p, f);
            }
        }
        _ => {}
    }
}

fn expr_contains_jsonpath_fts_match(expr: &ScalarExpr) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |part| {
        if expr_is_jsonpath_fts_match(part) {
            found = true;
        }
    });
    found
}

fn expr_is_jsonpath_fts_match(expr: &ScalarExpr) -> bool {
    matches!(
        expr,
        ScalarExpr::Func { name, args, .. }
            if name.eq_ignore_ascii_case("fts_match")
                && matches!(
                    args.get(1),
                    Some(ScalarExpr::Literal(Value::Str(path))) if path.trim_start().starts_with('$')
                )
    )
}

/// Iterate the recursive `CtePlan`: take the anchor (LHS of UNION ALL) as
/// the initial row set, then repeatedly evaluate the recursive step
/// (RHS) with the `CtePlan` bound to the *new rows from the previous
/// iteration* (working set), unioning the result back into the total.
/// Caps at 1024 iterations to keep buggy queries from running away.
fn materialize_recursive_cte(
    engine: &Engine,
    cte: &CtePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_filter: Option<&(String, ScalarExpr)>,
) -> Result<Vec<ResultRow>, SQLError> {
    if !cte.query.ctes.is_empty() {
        materialize_plan_ctes(engine, &cte.query.ctes, params, ctes)?;
    }

    let RelationalPlan::SetOp {
        kind,
        all,
        left,
        right,
        order_by,
        limit,
        offset,
        subqueries,
    } = &cte.query.root
    else {
        return Err(SQLError::Unsupported(
            "recursive CTE requires a UNION query".into(),
        ));
    };
    if *kind != SetOpKind::Union {
        return Err(SQLError::Unsupported(
            "recursive CTE only supports UNION".into(),
        ));
    }

    let declared_columns = (!cte.columns.is_empty()).then_some(cte.columns.as_slice());
    let (anchor_plan, step_plan) = if let Some((qualifier, filter)) = output_filter {
        let output_columns = declared_columns
            .map(<[String]>::to_vec)
            .or_else(|| query_plan_output_columns(left));
        match output_columns {
            Some(output_columns) => {
                let specialized_anchor = push_output_filter_into_query_plan(
                    engine,
                    left,
                    qualifier,
                    filter,
                    Some(&output_columns),
                );
                let specialized_step = push_output_filter_into_query_plan(
                    engine,
                    right,
                    qualifier,
                    filter,
                    Some(&output_columns),
                );
                match (specialized_anchor, specialized_step) {
                    (Some(anchor), Some(step)) => (anchor, step),
                    _ => ((**left).clone(), (**right).clone()),
                }
            }
            None => ((**left).clone(), (**right).clone()),
        }
    } else {
        ((**left).clone(), (**right).clone())
    };

    let anchor = execute_query_plan_with_ctes(engine, &anchor_plan, params, ctes)?;
    let anchor_columns = if cte.columns.is_empty() {
        anchor.columns.clone()
    } else {
        cte.columns.clone()
    };
    let mut working = apply_cte_column_aliases(anchor.rows, &anchor.columns, &anchor_columns);

    const MAX_ITERATIONS: usize = 1024;
    let rows = if *all {
        let mut accumulated = Vec::new();
        let mut iterations = 0usize;
        loop {
            if working.is_empty() {
                break accumulated;
            }
            if iterations == MAX_ITERATIONS {
                return Err(SQLError::Unsupported(format!(
                    "recursive CTE `{}` exceeded {MAX_ITERATIONS} iterations",
                    cte.name
                )));
            }
            iterations += 1;

            ctes.insert_materialized(cte.name.clone(), working);
            let step_result = execute_query_plan_with_ctes(engine, &step_plan, params, ctes);
            let previous = ctes.remove_materialized(&cte.name).unwrap_or_default();
            accumulated.extend(previous);
            let step = step_result?;
            working = apply_cte_column_aliases(step.rows, &step.columns, &anchor_columns);
        }
    } else {
        let mut accumulated = working.clone();
        let mut iterations = 0usize;
        loop {
            if working.is_empty() {
                break accumulated;
            }
            if iterations == MAX_ITERATIONS {
                return Err(SQLError::Unsupported(format!(
                    "recursive CTE `{}` exceeded {MAX_ITERATIONS} iterations",
                    cte.name
                )));
            }
            iterations += 1;

            ctes.insert_materialized(cte.name.clone(), working);
            let step_result = execute_query_plan_with_ctes(engine, &step_plan, params, ctes);
            ctes.remove_materialized(&cte.name);
            let step = step_result?;
            let renamed = apply_cte_column_aliases(step.rows, &step.columns, &anchor_columns);
            let next: Vec<_> = renamed
                .into_iter()
                .filter(|row| !accumulated.iter().any(|seen| seen == row))
                .collect();
            accumulated.extend(next.iter().cloned());
            working = next;
        }
    };

    if order_by.is_empty() && limit.is_none() && offset.is_none() {
        return Ok(rows);
    }
    let synthetic = QueryBlockPlan {
        projections: Vec::new(),
        from: None,
        r#where: None,
        compute: ComputePlan::Project,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        having: None,
        order_by: order_by.clone(),
        limit: limit.as_deref().cloned(),
        offset: offset.as_deref().cloned(),
        distinct: false,
        distinct_on: Vec::new(),
        subqueries: subqueries.clone(),
        access: AccessPathPlan::Row,
    };
    let mut ordering_scope = ctes.clone();
    ordering_scope.scalar_subqueries.clone_from(subqueries);
    apply_row_order_limit_with_ctes(rows, &synthetic, engine, params, &ordering_scope)
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
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
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
        if let ScalarExpr::Func {
            name,
            args,
            distinct,
            filter,
            ..
        } = &stmt.projections[0].expr
        {
            if name.eq_ignore_ascii_case("count")
                && matches!(args.as_slice(), [ScalarExpr::Star])
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

    let score_top_k = if matches!(
        block.access,
        AccessPathPlan::OperatorTree {
            score_limit_pushdown: true
        }
    ) {
        score_order_top_k(stmt, engine, params, ctes)?
            .filter(|_| score_limited_text_filter(stmt.r#where.as_ref()))
    } else {
        None
    };
    let has_jsonpath_fts_filter = stmt
        .r#where
        .as_ref()
        .is_some_and(expr_contains_jsonpath_fts_match);
    // Try the operator-tree pipeline first: lower the WHERE clause to
    // an `OperatorTree`, run `QueryOptimizer` (10 algebraic / graph-
    // aware / fusion-reordering passes - compatibility), then execute
    // through `PlanExecutor` against an `EngineDriver`. The bridge
    // returns `None` for shapes that are not posting-list access paths
    // (arithmetic across columns, subqueries, window calls, ...); those
    // remain scalar predicates in this relational filter node.
    let optimised = if has_jsonpath_fts_filter
        || !matches!(block.access, AccessPathPlan::OperatorTree { .. })
    {
        None
    } else if let (Some(top_k), Some(ScalarExpr::Func { name, args, .. })) =
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
            Some(filter_expr @ ScalarExpr::Func { name, args, .. })
                if uqa_sql::registry::is_registered(name)
                    && !expr_is_jsonpath_fts_match(filter_expr) =>
            {
                execute_function(engine, table, name, args, params)?
            }
            Some(filter_expr) => execute_mixed_where(engine, table, filter_expr, params)?,
        }
    };

    if matches!(&block.compute, ComputePlan::Aggregate) {
        let columns = projection_columns(&stmt.projections);
        let rows = build_aggregate_rows(engine, table, &scored, stmt, params, ctes)?;
        let rows = apply_row_order_limit_with_ctes(rows, stmt, engine, params, ctes)?;
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
        if let Some(result) = run_doc_ordered_select(engine, table, &scored, stmt, params, ctes)? {
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
        let rows = apply_row_order_limit_with_ctes(all_rows, stmt, engine, params, ctes)?;
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

    let scored = apply_order_limit(scored, stmt, engine, params, ctes)?;
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
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    let projection_hook = ScopedEngineHook::new(engine, ctes);
    let predicates = fdw_predicates_from_where(stmt.r#where.as_ref(), params);
    let scanned = engine
        .scan_foreign_table(table, None, &predicates, None)
        .map_err(SQLError::Unsupported)?;

    let filtered = if let Some(filter) = stmt.r#where.as_ref() {
        let mut out = Vec::with_capacity(scanned.len());
        for row in scanned {
            let ctx = PhysicalEvalContext::new(Some(&row), params)
                .with_function_hook(&projection_hook)
                .with_subquery_runner(&projection_hook);
            if uqa_sql::expr::truthy(&eval_physical_scalar(
                filter,
                &ctes.scalar_subqueries,
                &ctx,
            )?) {
                out.push(row);
            }
        }
        out
    } else {
        scanned
    };

    if matches!(&block.compute, ComputePlan::Aggregate) {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &filtered, params, ctes)?;
        let rows = apply_row_order_limit_with_ctes(rows, stmt, engine, params, ctes)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    if matches!(&block.compute, ComputePlan::Window) {
        let columns = projection_columns(&stmt.projections);
        let windowed = compute_window_columns(engine, &stmt.projections, filtered, params, ctes)?;
        let mut rows: Vec<ResultRow> = windowed
            .rows
            .iter()
            .map(|src| {
                project_join_row_with_plan(
                    engine,
                    &projection_hook,
                    &projection_hook,
                    &stmt.subqueries,
                    src,
                    &windowed.projections,
                    params,
                )
            })
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit_with_ctes(rows, stmt, engine, params, ctes)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let rows = volcano_project_sort_limit(engine, &filtered, stmt, params, ctes)?;
    let columns = expand_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        engine,
        Some(table),
    );
    Ok(SQLResult::from_rows(columns, rows))
}

fn fdw_predicates_from_where(
    expr: Option<&ScalarExpr>,
    params: &[SQLParam],
) -> Vec<uqa_fdw::FDWPredicate> {
    let Some(expr) = expr else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_fdw_predicates(expr, params, &mut out);
    out
}

fn collect_fdw_predicates(
    expr: &ScalarExpr,
    params: &[SQLParam],
    out: &mut Vec<uqa_fdw::FDWPredicate>,
) {
    match expr {
        ScalarExpr::And(parts) => {
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

fn fdw_predicate(expr: &ScalarExpr, params: &[SQLParam]) -> Option<uqa_fdw::FDWPredicate> {
    match expr {
        ScalarExpr::Binary { op, lhs, rhs } => {
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
        ScalarExpr::InList {
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
        ScalarExpr::IsNull { expr, negated } => Some(uqa_fdw::FDWPredicate {
            column: fdw_column_name(expr)?,
            operator: if *negated {
                uqa_fdw::PredicateOp::NotEq
            } else {
                uqa_fdw::PredicateOp::Eq
            },
            value: Value::Null,
        }),
        ScalarExpr::Func { name, args, .. } => fdw_like_predicate(name, args, false, params),
        ScalarExpr::Not(inner) => match inner.as_ref() {
            ScalarExpr::Func { name, args, .. } => fdw_like_predicate(name, args, true, params),
            _ => None,
        },
        _ => None,
    }
}

fn fdw_like_predicate(
    name: &str,
    args: &[ScalarExpr],
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

fn fdw_column_name(expr: &ScalarExpr) -> Option<String> {
    match expr {
        ScalarExpr::Column(name) => Some(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column.clone()),
        _ => None,
    }
}

fn fdw_const_value(expr: &ScalarExpr, params: &[SQLParam]) -> Option<Value> {
    let ctx = ScalarEvalContext::new(None, params);
    eval_scalar(expr, &ctx).ok()
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

fn facet_projection_fields(
    projections: &[ProjectionPlan],
) -> Result<Option<Vec<String>>, SQLError> {
    if projections.len() != 1 {
        return Ok(None);
    }
    let ScalarExpr::Func { name, args, .. } = &projections[0].expr else {
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

/// When a projection list contains `ScalarExpr::Star`, replace the synthetic
/// `*` placeholder in the result column list with the source schema.
/// Empty result sets still report the correct column shape, matching
/// `PostgreSQL`'s behaviour of `SELECT * FROM empty_table`.
pub(super) fn expand_star_columns(
    columns: Vec<String>,
    projections: &[ProjectionPlan],
    engine: &Engine,
    table: Option<&str>,
) -> Vec<String> {
    let has_star = projections
        .iter()
        .any(|p| matches!(p.expr, ScalarExpr::Star));
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

fn order_by_references_field(stmt: &QueryBlockPlan) -> bool {
    stmt.order_by.iter().any(|o| match &o.expr {
        ScalarExpr::Column(name) => name != SCORE_COLUMN,
        _ => true,
    })
}

/// Collect bare column names referenced by an ORDER BY expression.
/// Returns `false` (ineligible) when the expression contains anything
/// that cannot be resolved against a stored document alone: function
/// calls, subqueries, window calls, `*`, or a bare literal (which
/// `PostgreSQL` would treat as an output-ordinal reference).
fn collect_order_key_columns(expr: &ScalarExpr, out: &mut Vec<String>) -> bool {
    match expr {
        ScalarExpr::Column(name) => {
            out.push(name.clone());
            true
        }
        ScalarExpr::QualifiedColumn { column, .. } => {
            out.push(column.clone());
            true
        }
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => true,
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_order_key_columns(lhs, out) && collect_order_key_columns(rhs, out)
        }
        ScalarExpr::Not(inner) | ScalarExpr::Cast { expr: inner, .. } => {
            collect_order_key_columns(inner, out)
        }
        ScalarExpr::IsNull { expr, .. } => collect_order_key_columns(expr, out),
        ScalarExpr::Between { expr, low, high } => {
            collect_order_key_columns(expr, out)
                && collect_order_key_columns(low, out)
                && collect_order_key_columns(high, out)
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_order_key_columns(expr, out)
                && list.iter().all(|item| collect_order_key_columns(item, out))
        }
        ScalarExpr::And(items) | ScalarExpr::Or(items) | ScalarExpr::Array(items) => items
            .iter()
            .all(|item| collect_order_key_columns(item, out)),
        ScalarExpr::Case {
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
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<SQLResult>, SQLError> {
    use uqa_execution::relational::{compare_sort_key_values, SortKey};

    let columns = projection_columns(&stmt.projections);
    // Ordinal ORDER BY (bare integer literal) refers to the projected
    // output; leave it to the fallback.
    if stmt
        .order_by
        .iter()
        .any(|o| matches!(o.expr, ScalarExpr::Literal(_)))
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
            if label == name
                && !matches!(&stmt.projections[idx].expr, ScalarExpr::Column(c) if c == name)
            {
                return Ok(None);
            }
        }
    }
    let needs_score = referenced
        .iter()
        .any(|name| name == SCORE_COLUMN || name == DOC_ID_COLUMN);

    let resolved_offset =
        resolve_limit_offset_with_ctes(stmt.offset.as_ref(), engine, params, "OFFSET", ctes)?
            .unwrap_or(0) as usize;
    let resolved_limit =
        resolve_limit_offset_with_ctes(stmt.limit.as_ref(), engine, params, "LIMIT", ctes)?
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
            ScalarExpr::Column(name) if name == SCORE_COLUMN => Some(OrderKeySource::Score),
            ScalarExpr::Column(name) if name == DOC_ID_COLUMN => Some(OrderKeySource::DocId),
            ScalarExpr::Column(name) => key_fields
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
            let hook = ScopedEngineHook::new(engine, ctes);
            let ctx = PhysicalEvalContext::new(Some(&doc), params)
                .with_function_hook(&hook)
                .with_subquery_runner(&hook);
            let mut key_vals = Vec::with_capacity(keys.len());
            for key in &keys {
                key_vals.push(eval_physical_scalar(
                    &key.expr,
                    &ctes.scalar_subqueries,
                    &ctx,
                )?);
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

fn score_limited_text_filter(expr: Option<&ScalarExpr>) -> bool {
    let Some(ScalarExpr::Func { name, .. }) = expr else {
        return false;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "text_match" | "bayesian_match"
    )
}

fn score_order_top_k(
    stmt: &QueryBlockPlan,
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
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
    let Some(limit) =
        resolve_limit_offset_with_ctes(stmt.limit.as_ref(), engine, params, "LIMIT", ctes)?
    else {
        return Ok(None);
    };
    let offset =
        resolve_limit_offset_with_ctes(stmt.offset.as_ref(), engine, params, "OFFSET", ctes)?
            .unwrap_or(0);
    let top_k = usize::try_from(limit.saturating_add(offset)).unwrap_or(usize::MAX);
    Ok(Some(top_k))
}

pub(super) fn apply_row_order_limit_with_ctes(
    rows: Vec<ResultRow>,
    stmt: &QueryBlockPlan,
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
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
    let resolved_offset =
        resolve_limit_offset_with_ctes(stmt.offset.as_ref(), engine, params, "OFFSET", ctes)?;
    let resolved_limit =
        resolve_limit_offset_with_ctes(stmt.limit.as_ref(), engine, params, "LIMIT", ctes)?;
    if stmt.order_by.is_empty() && resolved_offset.is_none() && resolved_limit.is_none() {
        return Ok(rows);
    }

    // Materialise ORDER BY keys before entering the Volcano pipeline:
    // the Sort operator evaluates expressions without an engine hook,
    // so registered scalar functions would fail inside it.
    let mut rows = rows;
    if !stmt.order_by.is_empty() {
        let hook = ScopedEngineHook::new(engine, ctes);
        let key_values: Vec<Vec<Value>> = rows
            .iter()
            .map(|row| {
                let ctx = PhysicalEvalContext::new(Some(row), params)
                    .with_function_hook(&hook)
                    .with_subquery_runner(&hook);
                stmt.order_by
                    .iter()
                    .map(|order| eval_physical_scalar(&order.expr, &ctes.scalar_subqueries, &ctx))
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
                expr: ScalarExpr::Column(format!("{ORDER_KEY_PREFIX}{idx}")),
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

fn explain_int_expr(expr: &ScalarExpr) -> String {
    match expr {
        ScalarExpr::Literal(Value::Int(n)) => n.to_string(),
        _ => "<expr>".to_string(),
    }
}

/// Evaluate a `LIMIT` / `OFFSET` expression to a non-negative `u64`.
/// Mirrors the canonical UQA implementation's `_extract_int_value` - accepts integer constants,
/// `$N` parameter references, and any expression that the row-evaluator
/// can fold to an integer at execute time. Returns `None` when the
/// clause was absent.
fn resolve_limit_offset_with_ctes(
    expr: Option<&ScalarExpr>,
    engine: &Engine,
    params: &[SQLParam],
    label: &str,
    ctes: &CteScope,
) -> Result<Option<u64>, SQLError> {
    let Some(expr) = expr else {
        return Ok(None);
    };
    let hook = ScopedEngineHook::new(engine, ctes);
    let ctx = PhysicalEvalContext::new(None, params)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    let value = eval_physical_scalar(expr, &ctes.scalar_subqueries, &ctx)?;
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
    stmt: &QueryBlockPlan,
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
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
    if let Some(offset) =
        resolve_limit_offset_with_ctes(stmt.offset.as_ref(), engine, params, "OFFSET", ctes)?
    {
        let off = offset as usize;
        if off >= entries.len() {
            entries.clear();
        } else {
            entries.drain(0..off);
        }
    }
    if let Some(limit) =
        resolve_limit_offset_with_ctes(stmt.limit.as_ref(), engine, params, "LIMIT", ctes)?
    {
        entries.truncate(limit as usize);
    }
    Ok(entries)
}

pub(super) fn projection_columns(projections: &[ProjectionPlan]) -> Vec<String> {
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
    projections: &[ProjectionPlan],
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
    projections: &[ProjectionPlan],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let mut ctx = ScalarEvalContext::new(Some(document), params);
    if let Some(e) = engine {
        ctx = ctx.with_function_hook(e);
    }
    let labels = projection_columns(projections);
    let mut row = ResultRow::new();
    for (idx, proj) in projections.iter().enumerate() {
        let label = labels[idx].clone();
        if let ScalarExpr::Star = proj.expr {
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
        if let ScalarExpr::Func { name, args, .. } = &proj.expr {
            let mut evaluate = |expr: &ScalarExpr| eval_scalar(expr, &ctx);
            if let Some(value) = engine_func_intercept(engine, name, args, document, &mut evaluate)?
            {
                row.insert(label, value);
                continue;
            }
        }
        let value = eval_scalar(&proj.expr, &ctx)?;
        row.insert(label, value);
    }
    Ok(row)
}

pub(super) fn build_projection_row_with_ctes(
    engine: &Engine,
    document: &Document,
    projections: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<ResultRow, SQLError> {
    let hook = ScopedEngineHook::new(engine, ctes);
    let context = PhysicalEvalContext::new(Some(document), params)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    let labels = projection_columns(projections);
    let mut row = ResultRow::new();
    for (index, projection) in projections.iter().enumerate() {
        let label = labels[index].clone();
        if matches!(projection.expr, ScalarExpr::Star) {
            for (key, value) in document {
                if key.as_str() != SCORE_COLUMN
                    && key.as_str() != DOC_ID_COLUMN
                    && key.as_str() != MERGE_ACTION_COLUMN
                {
                    row.insert(key.clone(), value.clone());
                }
            }
            continue;
        }
        if let Some(value) = projected_value_from_row(&projection.expr, document) {
            row.insert(label, value);
            continue;
        }
        if let ScalarExpr::Func { name, args, .. } = &projection.expr {
            let mut evaluate =
                |expr: &ScalarExpr| eval_physical_scalar(expr, &ctes.scalar_subqueries, &context);
            if let Some(value) =
                engine_func_intercept(Some(engine), name, args, document, &mut evaluate)?
            {
                row.insert(label, value);
                continue;
            }
        }
        row.insert(
            label,
            eval_physical_scalar(&projection.expr, &ctes.scalar_subqueries, &context)?,
        );
    }
    Ok(row)
}
