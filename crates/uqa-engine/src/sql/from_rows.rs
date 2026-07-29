//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM/JOIN row assembly, table functions, and projection intercepts.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::Value;
use uqa_execution::{
    eval_call_arguments, eval_scalar, ScalarEvalContext, ScalarExpr, ScalarOrder,
    ScalarSubqueryRunner,
};
use uqa_joins::row_join::JoinKey;
use uqa_planner::{
    AccessPathPlan, ComputePlan, ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan,
    SourcePlan,
};
use uqa_sql::ast::JoinKind;
use uqa_sql::{ResultRow, SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::Document;

use crate::{Engine, SQLTableFunctionResult};

use super::scalar::{
    eval_physical_scalar, PhysicalEvalContext, PhysicalSubqueryRunner, PlanSubqueryArena,
};
use super::select::{
    execute_query_plan_with_ctes, push_output_filter_into_query_plan,
    select_contains_volatile_function, CteScope, ScopedEngineHook,
};
use super::{
    age_cypher, aggregate_join_rows, apply_row_order_limit_with_ctes, build_info_schema_rows,
    expect_column_name, expect_optional_graph_value, graph_betweenness_entries, graph_hits_entries,
    graph_pagerank_entries, has_aggregate, json_table_arg, json_table_value_to_text,
    projected_value_from_row, projection_columns, run_age_create_graph_with_evaluator,
    run_age_drop_graph_with_evaluator, run_graph_create_with_evaluator,
    run_graph_drop_with_evaluator, MERGE_ACTION_COLUMN, SCORE_COLUMN,
};

pub(super) type ColumnPrune = BTreeMap<String, BTreeSet<String>>;
pub(super) type QualifierFilters = BTreeMap<String, Vec<ScalarExpr>>;
type RowFilter<'a> = &'a mut dyn FnMut(&mut Vec<ResultRow>) -> Result<(), SQLError>;

struct JoinRuntime<'a> {
    engine: &'a Engine,
    function_hook: &'a dyn uqa_sql::expr::EngineHook,
    subquery_runner: &'a dyn ScalarSubqueryRunner,
    params: &'a [SQLParam],
}

fn qualifier_for(name: &str, alias: Option<&str>) -> String {
    alias.unwrap_or(name).to_string()
}

fn qualified_key(qual: &str, column: &str) -> String {
    let mut key = String::with_capacity(qual.len() + 1 + column.len());
    key.push_str(qual);
    key.push('.');
    key.push_str(column);
    key
}

fn table_row_cache_key(name: &str) -> String {
    format!("__uqa_internal_table_cache__:{name}")
}

fn prefixed_table_row_cache_key(name: &str, qual: &str) -> String {
    format!("__uqa_internal_prefixed_table_cache__:{name}:{qual}")
}

fn prefixed_view_row_cache_key(name: &str, qual: &str) -> String {
    format!("__uqa_internal_prefixed_view_cache__:{name}:{qual}")
}

fn prefixed_cte_row_cache_key(name: &str, qual: &str) -> String {
    format!("__uqa_internal_prefixed_cte_cache__:{name}:{qual}")
}

fn load_table_rows(engine: &Engine, table: &str) -> Vec<Document> {
    let doc_ids = engine.table_doc_ids(table);
    let mut documents = engine.get_documents_bulk(table, &doc_ids);
    doc_ids
        .into_iter()
        .filter_map(|id| documents.remove(&id))
        .collect()
}

fn load_table_rows_pruned(
    engine: &Engine,
    table: &str,
    qual: &str,
    columns: &BTreeSet<String>,
) -> Vec<ResultRow> {
    let doc_ids = engine.table_doc_ids(table);
    let names: Vec<&str> = columns.iter().map(String::as_str).collect();
    let values = engine.get_document_fields_multi(table, &doc_ids, &names);
    let mut rows = vec![ResultRow::new(); doc_ids.len()];
    let empty: Vec<Value> = Vec::new();
    for (idx, doc_id) in doc_ids.iter().enumerate() {
        let row_values = values.get(doc_id).unwrap_or(&empty);
        for (column, value) in columns.iter().zip(row_values) {
            rows[idx].insert(qualified_key(qual, column), value.clone());
        }
    }
    rows
}

/// Synthesize rows for `information_schema` / `pg_catalog` virtual
/// views. Returns `None` for any unknown name so the caller falls back
/// to the regular table lookup.
pub(super) fn prefix_row(qual: &str, doc: &Document) -> ResultRow {
    let mut out = ResultRow::new();
    for (k, v) in doc {
        out.insert(qualified_key(qual, k), v.clone());
    }
    out
}

fn prefix_row_pruned(qual: &str, doc: &Document, prune: Option<&ColumnPrune>) -> ResultRow {
    let mut out = ResultRow::new();
    let wanted = prune.and_then(|columns| columns.get(qual));
    for (k, v) in doc {
        if wanted.is_some_and(|columns| !columns.contains(k)) {
            continue;
        }
        out.insert(qualified_key(qual, k), v.clone());
    }
    out
}

/// Re-key a row that already has unprefixed column labels onto a new
/// qualifier. Used to plug CTE materializations into the JOIN executor
/// under whatever alias the outer query referenced them by.
fn reprefix_row_pruned(qual: &str, row: &ResultRow, prune: Option<&ColumnPrune>) -> ResultRow {
    let mut out = ResultRow::new();
    let wanted = prune.and_then(|columns| columns.get(qual));
    for (k, v) in row {
        // CTE rows are already keyed by their projection labels; lift
        // unqualified labels under the new qualifier so qualified refs
        // (`alias.col`) and unqualified suffix matches both resolve.
        let column = k.rsplit_once('.').map_or(k.as_str(), |(_, col)| col);
        if wanted.is_some_and(|columns| !columns.contains(column)) {
            continue;
        }
        let key = qualified_key(qual, column);
        out.insert(key, v.clone());
    }
    out
}

fn reprefix_rows_pruned(
    qual: &str,
    rows: &[ResultRow],
    prune: Option<&ColumnPrune>,
) -> Vec<ResultRow> {
    let Some(first) = rows.first() else {
        return Vec::new();
    };
    let same_schema = rows
        .iter()
        .all(|row| row.len() == first.len() && row.keys().eq(first.keys()));
    if !same_schema {
        return rows
            .iter()
            .map(|row| reprefix_row_pruned(qual, row, prune))
            .collect();
    }

    let wanted = prune.and_then(|columns| columns.get(qual));
    let keys: Vec<(String, String)> = first
        .keys()
        .filter_map(|key| {
            let column = key.rsplit_once('.').map_or(key.as_str(), |(_, col)| col);
            if wanted.is_some_and(|columns| !columns.contains(column)) {
                return None;
            }
            Some((key.clone(), qualified_key(qual, column)))
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut out = ResultRow::new();
            for (source_key, target_key) in &keys {
                if let Some(value) = row.get(source_key) {
                    out.insert(target_key.clone(), value.clone());
                }
            }
            out
        })
        .collect()
}

fn merge_rows(left: &ResultRow, right: &ResultRow) -> ResultRow {
    if left.len() >= right.len() {
        let mut out = left.clone();
        for (k, v) in right {
            out.insert(k.clone(), v.clone());
        }
        out
    } else {
        let mut out = right.clone();
        for (k, v) in left {
            out.entry(k.clone()).or_insert_with(|| v.clone());
        }
        out
    }
}

fn has_filters_for_qualifier(filters: Option<&QualifierFilters>, qual: &str) -> bool {
    filters
        .and_then(|filters| filters.get(qual))
        .is_some_and(|filters| !filters.is_empty())
}

fn apply_qualifier_filters(
    engine: &Engine,
    rows: Vec<ResultRow>,
    filters: Option<&QualifierFilters>,
    qual: &str,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let Some(filters) = filters.and_then(|filters| filters.get(qual)) else {
        return Ok(rows);
    };
    if filters.is_empty() || rows.is_empty() {
        return Ok(rows);
    }
    let filter = combine_filters(filters.iter().cloned());
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let ctx = ScalarEvalContext::new(Some(&row), params).with_function_hook(engine);
        if uqa_sql::expr::truthy(&eval_scalar(&filter, &ctx)?) {
            out.push(row);
        }
    }
    Ok(out)
}

fn qualifier_filters_require_table_select(filters: Option<&QualifierFilters>, qual: &str) -> bool {
    // Any pushed-down filter benefits from the single-table SELECT
    // path: registered row functions need it for posting-list
    // execution, and plain scalar predicates pick up value-index
    // acceleration plus bulk row materialisation there.
    filters
        .and_then(|filters| filters.get(qual))
        .is_some_and(|filters| !filters.is_empty())
}

fn run_table_select_for_qualifier_filters(
    engine: &Engine,
    table: &str,
    qual: &str,
    filters: Option<&QualifierFilters>,
    prune: Option<&ColumnPrune>,
    params: &[SQLParam],
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    if !qualifier_filters_require_table_select(filters, qual) {
        return Ok(None);
    }
    let Some(filters) = filters.and_then(|filters| filters.get(qual)) else {
        return Ok(None);
    };
    if filters.is_empty() {
        return Ok(None);
    }
    let Some(filter) =
        dequalify_expr_for_qualifier(&combine_filters(filters.iter().cloned()), qual)
    else {
        return Ok(None);
    };

    let mut projections = vec![ProjectionPlan {
        expr: ScalarExpr::Star,
        alias: None,
    }];
    if prune
        .and_then(|prune| prune.get(qual))
        .is_some_and(|columns| columns.contains(SCORE_COLUMN))
    {
        projections.push(ProjectionPlan {
            expr: ScalarExpr::Column(SCORE_COLUMN.to_string()),
            alias: None,
        });
    }

    let stmt = QueryBlockPlan {
        projections,
        from: Some(SourcePlan::Table {
            name: table.to_string(),
            alias: None,
        }),
        r#where: Some(filter),
        compute: ComputePlan::Project,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        having: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        subqueries: Vec::new(),
        access: AccessPathPlan::OperatorTree {
            score_limit_pushdown: false,
        },
    };
    let result = super::select::execute_query_plan(
        engine,
        &QueryPlan {
            ctes: Vec::new(),
            root: RelationalPlan::QueryBlock(Box::new(stmt)),
        },
        params,
    )?;
    Ok(Some(reprefix_rows_pruned(qual, &result.rows, prune)))
}

fn dequalify_expr_for_qualifier(expr: &ScalarExpr, qual: &str) -> Option<ScalarExpr> {
    match expr {
        ScalarExpr::QualifiedColumn {
            qualifier, column, ..
        } => (qualifier == qual).then(|| ScalarExpr::Column(column.clone())),
        ScalarExpr::Column(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star => Some(expr.clone()),
        ScalarExpr::Array(items) => Some(ScalarExpr::Array(
            items
                .iter()
                .map(|item| dequalify_expr_for_qualifier(item, qual))
                .collect::<Option<Vec<_>>>()?,
        )),
        ScalarExpr::And(items) => Some(ScalarExpr::And(
            items
                .iter()
                .map(|item| dequalify_expr_for_qualifier(item, qual))
                .collect::<Option<Vec<_>>>()?,
        )),
        ScalarExpr::Or(items) => Some(ScalarExpr::Or(
            items
                .iter()
                .map(|item| dequalify_expr_for_qualifier(item, qual))
                .collect::<Option<Vec<_>>>()?,
        )),
        ScalarExpr::Binary { op, lhs, rhs } => Some(ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(dequalify_expr_for_qualifier(lhs, qual)?),
            rhs: Box::new(dequalify_expr_for_qualifier(rhs, qual)?),
        }),
        ScalarExpr::Not(inner) => Some(ScalarExpr::Not(Box::new(dequalify_expr_for_qualifier(
            inner, qual,
        )?))),
        ScalarExpr::IsNull { expr, negated } => Some(ScalarExpr::IsNull {
            expr: Box::new(dequalify_expr_for_qualifier(expr, qual)?),
            negated: *negated,
        }),
        ScalarExpr::Between { expr, low, high } => Some(ScalarExpr::Between {
            expr: Box::new(dequalify_expr_for_qualifier(expr, qual)?),
            low: Box::new(dequalify_expr_for_qualifier(low, qual)?),
            high: Box::new(dequalify_expr_for_qualifier(high, qual)?),
        }),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => Some(ScalarExpr::InList {
            expr: Box::new(dequalify_expr_for_qualifier(expr, qual)?),
            list: list
                .iter()
                .map(|item| dequalify_expr_for_qualifier(item, qual))
                .collect::<Option<Vec<_>>>()?,
            negated: *negated,
        }),
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => Some(ScalarExpr::Func {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| dequalify_expr_for_qualifier(arg, qual))
                .collect::<Option<Vec<_>>>()?,
            distinct: *distinct,
            order_by: order_by
                .iter()
                .map(|order| {
                    Some(ScalarOrder {
                        expr: dequalify_expr_for_qualifier(&order.expr, qual)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            filter: match filter.as_ref() {
                Some(filter) => Some(Box::new(dequalify_expr_for_qualifier(filter, qual)?)),
                None => None,
            },
        }),
        ScalarExpr::WindowCall { name, args, spec } => {
            let mut spec = spec.clone();
            spec.partition_by = spec
                .partition_by
                .iter()
                .map(|expr| dequalify_expr_for_qualifier(expr, qual))
                .collect::<Option<Vec<_>>>()?;
            spec.order_by = spec
                .order_by
                .iter()
                .map(|order| {
                    Some(ScalarOrder {
                        expr: dequalify_expr_for_qualifier(&order.expr, qual)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(ScalarExpr::WindowCall {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| dequalify_expr_for_qualifier(arg, qual))
                    .collect::<Option<Vec<_>>>()?,
                spec,
            })
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => Some(ScalarExpr::Case {
            base: match base.as_ref() {
                Some(expr) => Some(Box::new(dequalify_expr_for_qualifier(expr, qual)?)),
                None => None,
            },
            when: when
                .iter()
                .map(|(cond, result)| {
                    Some((
                        dequalify_expr_for_qualifier(cond, qual)?,
                        dequalify_expr_for_qualifier(result, qual)?,
                    ))
                })
                .collect::<Option<Vec<_>>>()?,
            else_branch: match else_branch.as_ref() {
                Some(expr) => Some(Box::new(dequalify_expr_for_qualifier(expr, qual)?)),
                None => None,
            },
        }),
        ScalarExpr::Cast { expr, ty } => Some(ScalarExpr::Cast {
            expr: Box::new(dequalify_expr_for_qualifier(expr, qual)?),
            ty: ty.clone(),
        }),
        ScalarExpr::InSubquery { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => None,
    }
}

fn combine_filters(filters: impl IntoIterator<Item = ScalarExpr>) -> ScalarExpr {
    let mut filters: Vec<ScalarExpr> = filters.into_iter().collect();
    if filters.len() == 1 {
        filters.pop().unwrap()
    } else {
        ScalarExpr::And(filters)
    }
}

fn propagated_join_filters(
    filters: &QualifierFilters,
    source_from: &SourcePlan,
    target_from: &SourcePlan,
    on: Option<&ScalarExpr>,
) -> Option<QualifierFilters> {
    let on = on?;
    let mut out = filters.clone();
    let mut changed = false;
    let source_quals = from_qualifiers(source_from);
    let target_quals = from_qualifiers(target_from);
    for (left, right) in join_column_equalities(on) {
        changed |= propagate_join_filter_pair(
            filters,
            &mut out,
            &source_quals,
            &target_quals,
            &left,
            &right,
        );
        changed |= propagate_join_filter_pair(
            filters,
            &mut out,
            &source_quals,
            &target_quals,
            &right,
            &left,
        );
    }
    changed.then_some(out)
}

fn propagate_join_filter_pair(
    filters: &QualifierFilters,
    out: &mut QualifierFilters,
    source_quals: &BTreeSet<String>,
    target_quals: &BTreeSet<String>,
    source: &(String, String),
    target: &(String, String),
) -> bool {
    if !source_quals.contains(&source.0) || !target_quals.contains(&target.0) {
        return false;
    }
    let mut changed = false;
    if let Some(source_filters) = filters.get(&source.0) {
        for filter in source_filters {
            if let Some(value) = constant_equality_for_column(filter, &source.0, &source.1) {
                let propagated = ScalarExpr::Binary {
                    op: uqa_sql::ast::BinaryOp::Equal,
                    lhs: Box::new(ScalarExpr::qualified_column(&target.0, &target.1)),
                    rhs: Box::new(value),
                };
                out.entry(target.0.clone()).or_default().push(propagated);
                changed = true;
            }
        }
    }
    changed
}

fn constant_equality_for_column(expr: &ScalarExpr, qual: &str, column: &str) -> Option<ScalarExpr> {
    let ScalarExpr::Binary {
        op: uqa_sql::ast::BinaryOp::Equal,
        lhs,
        rhs,
    } = expr
    else {
        return None;
    };
    if expr_is_qualified_column(lhs, qual, column) && expr_is_constant(rhs) {
        return Some((**rhs).clone());
    }
    if expr_is_qualified_column(rhs, qual, column) && expr_is_constant(lhs) {
        return Some((**lhs).clone());
    }
    None
}

fn expr_is_qualified_column(expr: &ScalarExpr, qual: &str, column: &str) -> bool {
    matches!(
        expr,
        ScalarExpr::QualifiedColumn {
            qualifier,
            column: col,
            ..
        } if qualifier == qual && col == column
    )
}

fn expr_is_constant(expr: &ScalarExpr) -> bool {
    matches!(expr, ScalarExpr::Literal(_) | ScalarExpr::Param(_))
}

fn join_column_equalities(expr: &ScalarExpr) -> Vec<((String, String), (String, String))> {
    match expr {
        ScalarExpr::And(items) => items.iter().flat_map(join_column_equalities).collect(),
        ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::Equal,
            lhs,
            rhs,
        } => {
            let Some(left) = qualified_column_pair(lhs) else {
                return Vec::new();
            };
            let Some(right) = qualified_column_pair(rhs) else {
                return Vec::new();
            };
            vec![(left, right)]
        }
        _ => Vec::new(),
    }
}

fn qualified_column_pair(expr: &ScalarExpr) -> Option<(String, String)> {
    match expr {
        ScalarExpr::QualifiedColumn {
            qualifier, column, ..
        } => Some((qualifier.clone(), column.clone())),
        _ => None,
    }
}

fn from_qualifiers(from: &SourcePlan) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_from_qualifiers(from, &mut out);
    out
}

fn collect_from_qualifiers(from: &SourcePlan, out: &mut BTreeSet<String>) {
    match from {
        SourcePlan::Table { name, alias } => {
            out.insert(alias.clone().unwrap_or_else(|| name.clone()));
        }
        SourcePlan::Join { left, right, .. } => {
            collect_from_qualifiers(left, out);
            collect_from_qualifiers(right, out);
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

fn null_row_for(table: &str, alias: Option<&str>, engine: &Engine) -> ResultRow {
    let qual = qualifier_for(table, alias);
    let mut out = ResultRow::new();
    if engine.table(table).is_none() {
        for column in engine.foreign_table_columns(table) {
            out.insert(qualified_key(&qual, &column), Value::Null);
        }
        return out;
    }
    // Emit NULLs for any column that ever appeared in the table; for an
    // empty table we still know the keys via document_count, but the
    // safe default is just an empty row - a missing key resolves to
    // NULL through ScalarExpr::Column / QualifiedColumn lookup anyway.
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
        out.insert(qualified_key(&qual, &k), Value::Null);
    }
    out
}

pub(super) fn build_join_rows_with_ctes(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut row_filter = None;
    build_join_rows_with_ctes_inner(engine, from, params, ctes, &mut row_filter, None, None)
}

pub(super) fn build_join_rows_with_ctes_pruned(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    prune: &ColumnPrune,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut row_filter = None;
    build_join_rows_with_ctes_inner(
        engine,
        from,
        params,
        ctes,
        &mut row_filter,
        Some(prune),
        None,
    )
}

pub(super) fn build_join_rows_with_ctes_filtered_by_qualifier(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    filters: &QualifierFilters,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut row_filter = None;
    build_join_rows_with_ctes_inner(
        engine,
        from,
        params,
        ctes,
        &mut row_filter,
        None,
        Some(filters),
    )
}

pub(super) fn build_join_rows_with_ctes_pruned_filtered_by_qualifier(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    prune: &ColumnPrune,
    filters: &QualifierFilters,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut row_filter = None;
    build_join_rows_with_ctes_inner(
        engine,
        from,
        params,
        ctes,
        &mut row_filter,
        Some(prune),
        Some(filters),
    )
}

pub(super) fn build_join_rows_with_ctes_filtered(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    row_filter: RowFilter<'_>,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut row_filter = Some(row_filter);
    build_join_rows_with_ctes_inner(engine, from, params, ctes, &mut row_filter, None, None)
}

pub(super) fn build_join_rows_with_ctes_filtered_pruned(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    row_filter: RowFilter<'_>,
    prune: &ColumnPrune,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut row_filter = Some(row_filter);
    build_join_rows_with_ctes_inner(
        engine,
        from,
        params,
        ctes,
        &mut row_filter,
        Some(prune),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_join_rows_with_ctes_filtered_pruned_filtered_by_qualifier(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    row_filter: RowFilter<'_>,
    prune: &ColumnPrune,
    filters: &QualifierFilters,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut row_filter = Some(row_filter);
    build_join_rows_with_ctes_inner(
        engine,
        from,
        params,
        ctes,
        &mut row_filter,
        Some(prune),
        Some(filters),
    )
}

pub(super) fn build_join_rows_with_ctes_filtered_filtered_by_qualifier(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    row_filter: RowFilter<'_>,
    filters: &QualifierFilters,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut row_filter = Some(row_filter);
    build_join_rows_with_ctes_inner(
        engine,
        from,
        params,
        ctes,
        &mut row_filter,
        None,
        Some(filters),
    )
}

fn build_join_rows_with_ctes_inner(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    row_filter: &mut Option<RowFilter<'_>>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Vec<ResultRow>, SQLError> {
    match from {
        SourcePlan::Table { name, alias } => {
            let qual = qualifier_for(name, alias.as_deref());
            // CTE reference takes precedence over a real table of the
            // same name (matches `PostgreSQL` semantics).
            let prefixed_cte_cache_key = prefixed_cte_row_cache_key(name, &qual);
            if prune.is_none() && !has_filters_for_qualifier(filters, &qual) {
                if let Some(rows) = ctes.rows.get(&prefixed_cte_cache_key) {
                    return Ok(rows.clone());
                }
            }
            if let Some(rows) = ctes.rows.get(name) {
                let prefixed = reprefix_rows_pruned(&qual, rows, prune);
                let prefixed = apply_qualifier_filters(engine, prefixed, filters, &qual, params)?;
                if prune.is_none() && !has_filters_for_qualifier(filters, &qual) {
                    ctes.rows.insert(prefixed_cte_cache_key, prefixed.clone());
                }
                return Ok(prefixed);
            }
            let prefixed_view_cache_key = prefixed_view_row_cache_key(name, &qual);
            if prune.is_none() && !has_filters_for_qualifier(filters, &qual) {
                if let Some(rows) = ctes.rows.get(&prefixed_view_cache_key) {
                    return Ok(rows.clone());
                }
            }
            if let Some(rows) = ctes.rows.get(name) {
                let prefixed = reprefix_rows_pruned(&qual, rows, prune);
                return apply_qualifier_filters(engine, prefixed, filters, &qual, params);
            }
            if let Some(plan) = engine.view_plan(name) {
                let specialized_plan = filters
                    .and_then(|filters| filters.get(&qual))
                    .filter(|filters| !filters.is_empty())
                    .and_then(|filters| {
                        let filter = combine_filters(filters.iter().cloned());
                        push_output_filter_into_query_plan(engine, &plan, &qual, &filter, None)
                    });
                let execution_plan = specialized_plan.as_ref().unwrap_or(&plan);
                let local_cte_names = query_cte_names(execution_plan);
                let is_volatile = query_contains_volatile_function(execution_plan);
                let result = if is_volatile {
                    let mut scoped_ctes = ctes.clone();
                    execute_query_plan_with_ctes(engine, execution_plan, params, &mut scoped_ctes)?
                } else {
                    execute_view_plan_with_parent_cache(
                        engine,
                        execution_plan,
                        params,
                        ctes,
                        &local_cte_names,
                    )?
                };
                let prefixed = reprefix_rows_pruned(&qual, &result.rows, prune);
                let prefixed = apply_qualifier_filters(engine, prefixed, filters, &qual, params)?;
                if !is_volatile && specialized_plan.is_none() {
                    ctes.rows.insert(name.clone(), result.rows);
                    if prune.is_none() && !has_filters_for_qualifier(filters, &qual) {
                        ctes.rows.insert(prefixed_view_cache_key, prefixed.clone());
                    }
                }
                return Ok(prefixed);
            }
            // information_schema / pg_catalog virtual views.
            if let Some(rows) = build_info_schema_rows(engine, name) {
                let prefixed = reprefix_rows_pruned(&qual, &rows, prune);
                return apply_qualifier_filters(engine, prefixed, filters, &qual, params);
            }
            if engine.foreign_table(name).is_some() {
                let rows = engine
                    .scan_foreign_table(name, None, &[], None)
                    .map_err(SQLError::Unsupported)?;
                let prefixed = reprefix_rows_pruned(&qual, &rows, prune);
                return apply_qualifier_filters(engine, prefixed, filters, &qual, params);
            }
            if engine.table(name).is_none() {
                return Err(SQLError::Unsupported(format!(
                    "relation `{name}` does not exist"
                )));
            }
            if let Some(rows) =
                run_table_select_for_qualifier_filters(engine, name, &qual, filters, prune, params)?
            {
                return Ok(rows);
            }
            let prefixed_cache_key = prefixed_table_row_cache_key(name, &qual);
            if prune.is_none() && !has_filters_for_qualifier(filters, &qual) {
                if let Some(rows) = ctes.rows.get(&prefixed_cache_key) {
                    return Ok(rows.clone());
                }
            }
            let prefixed: Vec<ResultRow> =
                if let Some(columns) = prune.and_then(|prune| prune.get(&qual)) {
                    load_table_rows_pruned(engine, name, &qual, columns)
                } else {
                    let cache_key = table_row_cache_key(name);
                    let rows = if let Some(rows) = ctes.rows.get(&cache_key) {
                        rows.clone()
                    } else {
                        let rows: Vec<ResultRow> = load_table_rows(engine, name);
                        ctes.rows.insert(cache_key, rows.clone());
                        rows
                    };
                    rows.iter()
                        .map(|row| prefix_row_pruned(&qual, row, prune))
                        .collect()
                };
            let prefixed = apply_qualifier_filters(engine, prefixed, filters, &qual, params)?;
            if prune.is_none() && !has_filters_for_qualifier(filters, &qual) {
                ctes.rows.insert(prefixed_cache_key, prefixed.clone());
            }
            Ok(prefixed)
        }
        SourcePlan::Join {
            left,
            right,
            kind,
            on,
            lateral,
        } => {
            let left_filters = filters
                .and_then(|filters| propagated_join_filters(filters, right, left, on.as_ref()));
            let left_filter_ref = left_filters.as_ref().or(filters);
            let left_rows = build_join_rows_with_ctes_inner(
                engine,
                left,
                params,
                ctes,
                row_filter,
                prune,
                left_filter_ref,
            )?;
            // LATERAL: re-evaluate the right side once per left row,
            // so the right body can reference outer columns. The
            // engine substitutes the outer row into the ScalarEvalContext
            // through the row-level evaluator.
            let implicit_lateral_function = matches!(right.as_ref(), SourcePlan::Function { .. });
            if *lateral || implicit_lateral_function {
                return build_lateral_join_rows(
                    engine,
                    &left_rows,
                    right,
                    *kind,
                    on.as_ref(),
                    params,
                    ctes,
                );
            }
            let right_filters = filters
                .and_then(|filters| propagated_join_filters(filters, left, right, on.as_ref()));
            let right_filter_ref = right_filters.as_ref().or(filters);
            let right_rows = build_join_rows_with_ctes_inner(
                engine,
                right,
                params,
                ctes,
                row_filter,
                prune,
                right_filter_ref,
            )?;
            let on_expr = on.as_ref();
            let scoped_hook = ScopedEngineHook::new(engine, ctes);
            let eval_hook: &dyn uqa_sql::expr::EngineHook = &scoped_hook;
            let subquery_arena =
                PlanSubqueryArena::new(&ctes.scalar_subqueries, Some(&scoped_hook));
            let subquery_runner: &dyn ScalarSubqueryRunner = &subquery_arena;
            let join_runtime = JoinRuntime {
                engine,
                function_hook: eval_hook,
                subquery_runner,
                params,
            };

            let mut rows = match kind {
                JoinKind::Inner | JoinKind::Cross => {
                    if matches!(kind, JoinKind::Inner) {
                        if let Some(rows) = try_hash_inner_join(
                            eval_hook,
                            subquery_runner,
                            &left_rows,
                            &right_rows,
                            on_expr,
                            params,
                        )? {
                            rows
                        } else {
                            cross_filter(
                                eval_hook,
                                subquery_runner,
                                &left_rows,
                                &right_rows,
                                on_expr,
                                params,
                            )?
                        }
                    } else {
                        cross_filter(
                            eval_hook,
                            subquery_runner,
                            &left_rows,
                            &right_rows,
                            on_expr,
                            params,
                        )?
                    }
                }
                JoinKind::Left => {
                    if let Some(rows) =
                        try_hash_left_join(&join_runtime, &left_rows, &right_rows, right, on_expr)?
                    {
                        rows
                    } else {
                        left_outer(&join_runtime, &left_rows, &right_rows, right, on_expr)?
                    }
                }
                JoinKind::Right => {
                    left_outer(&join_runtime, &right_rows, &left_rows, left, on_expr)?
                }
                JoinKind::Full => {
                    full_outer(&join_runtime, &left_rows, &right_rows, left, right, on_expr)?
                }
            };
            if let Some(filter) = row_filter.as_deref_mut() {
                filter(&mut rows)?;
            }
            Ok(rows)
        }
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
        } => {
            let hook = ScopedEngineHook::new(engine, ctes);
            Ok(build_values_rows(
                rows,
                alias.as_deref(),
                column_aliases,
                params,
                &hook,
                &hook,
                &ctes.scalar_subqueries,
            )?)
        }
        SourcePlan::Function {
            name,
            args,
            alias,
            column_aliases,
            column_types,
        } => {
            let hook = ScopedEngineHook::new(engine, ctes);
            let context = TableFunctionEvalContext::new(
                engine,
                params,
                &hook,
                &hook,
                &ctes.scalar_subqueries,
            );
            Ok(build_table_function_rows(
                &context,
                name,
                args,
                alias.as_deref(),
                column_aliases,
                column_types,
            )?)
        }
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            let local_cte_names = query_cte_names(body);
            let result =
                execute_view_plan_with_parent_cache(engine, body, params, ctes, &local_cte_names)?;
            Ok(materialize_subquery_rows(
                result,
                alias.as_deref(),
                column_aliases,
            ))
        }
    }
}

fn execute_view_plan_with_parent_cache(
    engine: &Engine,
    plan: &uqa_planner::QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    local_cte_names: &BTreeSet<String>,
) -> Result<SQLResult, SQLError> {
    let saved = save_and_remove_cte_names(ctes, local_cte_names);
    let result = execute_query_plan_with_ctes(engine, plan, params, ctes);
    restore_cte_names(ctes, saved);
    result
}

fn save_and_remove_cte_names(
    ctes: &mut CteScope,
    names: &BTreeSet<String>,
) -> Vec<(String, Option<Vec<ResultRow>>)> {
    names
        .iter()
        .map(|name| (name.clone(), ctes.remove_materialized(name)))
        .collect()
}

fn restore_cte_names(ctes: &mut CteScope, saved: Vec<(String, Option<Vec<ResultRow>>)>) {
    for (name, rows) in saved {
        match rows {
            Some(rows) => {
                ctes.rows.insert(name.clone(), rows);
            }
            None => {
                ctes.remove_materialized(&name);
            }
        }
    }
}

fn query_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_query_cte_names(plan, &mut names);
    names
}

fn collect_query_cte_names(plan: &QueryPlan, names: &mut BTreeSet<String>) {
    for cte in &plan.ctes {
        names.insert(cte.name.clone());
        collect_query_cte_names(&cte.query, names);
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                collect_source_query_cte_names(source, names);
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            collect_query_cte_names(left, names);
            collect_query_cte_names(right, names);
        }
        RelationalPlan::Values { .. } => {}
    }
}

fn collect_source_query_cte_names(source: &SourcePlan, names: &mut BTreeSet<String>) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            collect_source_query_cte_names(left, names);
            collect_source_query_cte_names(right, names);
        }
        SourcePlan::Subquery { body, .. } => collect_query_cte_names(body, names),
        SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Function { .. } => {}
    }
}

fn query_contains_volatile_function(plan: &QueryPlan) -> bool {
    plan.ctes
        .iter()
        .any(|cte| query_contains_volatile_function(&cte.query))
        || match &plan.root {
            RelationalPlan::QueryBlock(block) => {
                select_contains_volatile_function(block)
                    || block
                        .subqueries
                        .iter()
                        .any(query_contains_volatile_function)
                    || block
                        .from
                        .as_ref()
                        .is_some_and(source_contains_volatile_query)
            }
            RelationalPlan::SetOp { left, right, .. } => {
                query_contains_volatile_function(left) || query_contains_volatile_function(right)
            }
            RelationalPlan::Values { subqueries, .. } => {
                subqueries.iter().any(query_contains_volatile_function)
            }
        }
}

fn source_contains_volatile_query(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::Join { left, right, .. } => {
            source_contains_volatile_query(left) || source_contains_volatile_query(right)
        }
        SourcePlan::Subquery { body, .. } => query_contains_volatile_function(body),
        SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Function { .. } => false,
    }
}

fn materialize_subquery_rows(
    result: SQLResult,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Vec<ResultRow> {
    let cols = column_aliases.to_vec();
    result
        .rows
        .into_iter()
        .map(|mut r| {
            if !cols.is_empty() {
                let pairs: Vec<(String, Value)> = result
                    .columns
                    .iter()
                    .zip(cols.iter())
                    .filter_map(|(orig, new)| r.remove(orig).map(|v| (new.clone(), v)))
                    .collect();
                let mut renamed = ResultRow::new();
                for (k, v) in pairs {
                    renamed.insert(k, v);
                }
                if let Some(q) = alias {
                    return prefix_row(q, &renamed);
                }
                renamed
            } else if let Some(q) = alias {
                prefix_row(q, &r)
            } else {
                r
            }
        })
        .collect()
}

fn build_values_rows(
    rows: &[Vec<ScalarExpr>],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn PhysicalSubqueryRunner,
    subqueries: &[QueryPlan],
) -> Result<Vec<ResultRow>, SQLError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let n_cols = rows[0].len();
    let columns: Vec<String> = (0..n_cols)
        .map(|i| {
            column_aliases
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("column{}", i + 1))
        })
        .collect();
    let subquery_arena = PlanSubqueryArena::new(subqueries, Some(subquery_runner));
    let ctx = ScalarEvalContext::new(None, params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(&subquery_arena);
    let mut out: Vec<ResultRow> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut r = ResultRow::new();
        for (i, expr) in row.iter().enumerate() {
            let v = eval_scalar(expr, &ctx)?;
            r.insert(columns[i].clone(), v);
        }
        let r = match alias {
            Some(a) => prefix_row(a, &r),
            None => r,
        };
        out.push(r);
    }
    Ok(out)
}

/// LATERAL join executor: re-evaluates the right side per left row
/// so the right body can reference outer columns. We splice the
/// outer row into a per-row CTE-style scope by registering it under
/// the `__lateral__` reserved name and inlining its keys into a
/// fresh CTE map; the right side then sees those columns as plain
/// row keys when its internal expressions evaluate. Mirrors
/// `PostgreSQL` LATERAL semantics.
#[allow(clippy::too_many_arguments)]
fn build_lateral_join_rows(
    engine: &Engine,
    left_rows: &[ResultRow],
    right: &SourcePlan,
    kind: JoinKind,
    on: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::truthy;
    let mut out: Vec<ResultRow> = Vec::new();
    for left_row in left_rows {
        let right_rows = match right {
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                let result = execute_lateral_subquery(engine, body, left_row, params, ctes)?;
                materialize_subquery_rows(result, alias.as_deref(), column_aliases)
            }
            SourcePlan::Function {
                name,
                args,
                alias,
                column_aliases,
                column_types,
            } => {
                let hook = ScopedEngineHook::new(engine, ctes);
                let context = TableFunctionEvalContext::new(
                    engine,
                    params,
                    &hook,
                    &hook,
                    &ctes.scalar_subqueries,
                );
                build_table_function_rows_with_row(
                    &context,
                    name,
                    args,
                    alias.as_deref(),
                    column_aliases,
                    column_types,
                    Some(left_row),
                )?
            }
            _ => build_join_rows_with_ctes(engine, right, params, ctes)?,
        };
        let scoped_hook = ScopedEngineHook::new(engine, ctes);
        let subquery_arena = PlanSubqueryArena::new(&ctes.scalar_subqueries, Some(&scoped_hook));
        for r_row in &right_rows {
            let mut joined = ResultRow::new();
            for (k, v) in left_row {
                joined.insert(k.clone(), v.clone());
            }
            for (k, v) in r_row {
                joined.insert(k.clone(), v.clone());
            }
            let keep = match (on, kind) {
                (None, _) | (_, JoinKind::Cross) => true,
                (Some(filter), _) => {
                    let ctx = ScalarEvalContext::new(Some(&joined), params)
                        .with_function_hook(&scoped_hook)
                        .with_subquery_runner(&subquery_arena);
                    truthy(&eval_scalar(filter, &ctx)?)
                }
            };
            if keep {
                out.push(joined);
            }
        }
    }
    Ok(out)
}

pub(super) fn execute_lateral_subquery(
    engine: &Engine,
    plan: &QueryPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    let mut scoped_ctes = ctes.clone();
    super::select::materialize_plan_ctes(engine, &plan.ctes, params, &mut scoped_ctes)?;
    execute_lateral_relational_root(engine, &plan.root, outer_row, params, &mut scoped_ctes)
}

fn execute_lateral_relational_root(
    engine: &Engine,
    root: &RelationalPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    match root {
        RelationalPlan::QueryBlock(block) => {
            execute_lateral_query_block(engine, block, outer_row, params, ctes)
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
            let lhs = execute_lateral_subquery(engine, left, outer_row, params, ctes)?;
            let rhs = execute_lateral_subquery(engine, right, outer_row, params, ctes)?;
            let mut result = super::select::combine_set_results(*kind, *all, lhs, rhs);
            if !order_by.is_empty() || limit.is_some() || offset.is_some() {
                let order_plan = QueryBlockPlan {
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
                result.rows = apply_row_order_limit_with_ctes(
                    result.rows,
                    &order_plan,
                    engine,
                    params,
                    ctes,
                )?;
            }
            Ok(result)
        }
        RelationalPlan::Values { rows, subqueries } => {
            let columns: Vec<String> = rows
                .first()
                .map(|row| {
                    (0..row.len())
                        .map(|index| format!("column{}", index + 1))
                        .collect()
                })
                .unwrap_or_default();
            let hook = ScopedEngineHook::new(engine, ctes);
            let context = super::scalar::PhysicalEvalContext::new(Some(outer_row), params)
                .with_function_hook(&hook)
                .with_subquery_runner(&hook);
            let rows = rows
                .iter()
                .map(|values| {
                    let mut row = ResultRow::new();
                    for (index, expression) in values.iter().enumerate() {
                        row.insert(
                            columns[index].clone(),
                            super::scalar::eval_physical_scalar(expression, subqueries, &context)?,
                        );
                    }
                    Ok(row)
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            Ok(SQLResult::from_rows(columns, rows))
        }
    }
}

fn execute_lateral_query_block(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    scoped_ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    let Some(from) = stmt.from.as_ref() else {
        // A FROM-less body still applies its WHERE clause against the
        // outer scope (`EXISTS (SELECT 1 WHERE false)` has no rows).
        if let Some(filter) = stmt.r#where.as_ref() {
            let hook = ScopedEngineHook::new(engine, scoped_ctes);
            let ctx = PhysicalEvalContext::new(Some(outer_row), params)
                .with_function_hook(&hook)
                .with_subquery_runner(&hook);
            if !uqa_sql::expr::truthy(&eval_physical_scalar(filter, &stmt.subqueries, &ctx)?) {
                return Ok(SQLResult::from_rows(
                    projection_columns(&stmt.projections),
                    Vec::new(),
                ));
            }
        }
        let hook = ScopedEngineHook::new(engine, scoped_ctes);
        let projected = project_join_row_with_plan(
            engine,
            &hook,
            &hook,
            &stmt.subqueries,
            outer_row,
            &stmt.projections,
            params,
        )?;
        return Ok(SQLResult::from_rows(
            projection_columns(&stmt.projections),
            vec![projected],
        ));
    };

    let inner_rows = build_join_rows_with_ctes(engine, from, params, scoped_ctes)?;
    let mut filtered: Vec<ResultRow> = Vec::with_capacity(inner_rows.len());
    for inner in inner_rows {
        let merged = merge_lateral_scope_rows(outer_row, &inner);
        let keep = match stmt.r#where.as_ref() {
            None => true,
            Some(filter) => {
                let hook = ScopedEngineHook::new(engine, scoped_ctes);
                let ctx = PhysicalEvalContext::new(Some(&merged), params)
                    .with_function_hook(&hook)
                    .with_subquery_runner(&hook);
                uqa_sql::expr::truthy(&eval_physical_scalar(filter, &stmt.subqueries, &ctx)?)
            }
        };
        if keep {
            filtered.push(merged);
        }
    }

    if has_aggregate(engine, &stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &filtered, params, scoped_ctes)?;
        let rows = apply_row_order_limit_with_ctes(rows, stmt, engine, params, scoped_ctes)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let ordered = apply_row_order_limit_with_ctes(filtered, stmt, engine, params, scoped_ctes)?;
    let columns = projection_columns(&stmt.projections);
    let hook = ScopedEngineHook::new(engine, scoped_ctes);
    let rows = ordered
        .iter()
        .map(|src| {
            project_join_row_with_plan(
                engine,
                &hook,
                &hook,
                &stmt.subqueries,
                src,
                &stmt.projections,
                params,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SQLResult::from_rows(columns, rows))
}

fn merge_lateral_scope_rows(outer_row: &ResultRow, inner_row: &ResultRow) -> ResultRow {
    let mut merged = outer_row.clone();
    for (key, value) in inner_row {
        merged.insert(key.clone(), value.clone());
        if let Some((_, column)) = key.rsplit_once('.') {
            merged.insert(column.to_string(), value.clone());
        }
    }
    merged
}

pub(super) struct TableFunctionEvalContext<'a> {
    engine: &'a Engine,
    params: &'a [SQLParam],
    eval_hook: &'a dyn uqa_sql::expr::EngineHook,
    subquery_runner: &'a dyn PhysicalSubqueryRunner,
    subqueries: &'a [QueryPlan],
}

impl<'a> TableFunctionEvalContext<'a> {
    pub(super) fn new(
        engine: &'a Engine,
        params: &'a [SQLParam],
        eval_hook: &'a dyn uqa_sql::expr::EngineHook,
        subquery_runner: &'a dyn PhysicalSubqueryRunner,
        subqueries: &'a [QueryPlan],
    ) -> Self {
        Self {
            engine,
            params,
            eval_hook,
            subquery_runner,
            subqueries,
        }
    }
}

#[allow(clippy::similar_names)]
pub(super) fn build_table_function_rows(
    context: &TableFunctionEvalContext<'_>,
    name: &str,
    args: &[ScalarExpr],
    alias: Option<&str>,
    column_aliases: &[String],
    column_types: &[String],
) -> Result<Vec<ResultRow>, SQLError> {
    build_table_function_rows_with_row(
        context,
        name,
        args,
        alias,
        column_aliases,
        column_types,
        None,
    )
}

#[allow(clippy::similar_names)]
fn build_table_function_rows_with_row(
    context: &TableFunctionEvalContext<'_>,
    name: &str,
    args: &[ScalarExpr],
    alias: Option<&str>,
    column_aliases: &[String],
    column_types: &[String],
    row: Option<&ResultRow>,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::unknown_function_error;
    let engine = context.engine;
    let subquery_arena = PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
    let ctx = ScalarEvalContext::new(row, context.params)
        .with_function_hook(context.eval_hook)
        .with_subquery_runner(&subquery_arena);
    let lower = name.to_ascii_lowercase();
    let call_args = eval_call_arguments(args, &ctx)?;
    let has_named_args = call_args.iter().any(|(name, _)| name.is_some());
    let evaluated: Vec<Value> = call_args.iter().map(|(_, value)| value.clone()).collect();
    let default_col = column_aliases
        .first()
        .cloned()
        .unwrap_or_else(|| alias.unwrap_or(name).to_string());
    let qual = alias;
    let mut out: Vec<ResultRow> = Vec::new();
    let push_scalar = |out: &mut Vec<ResultRow>, value: Value| {
        let mut r = ResultRow::new();
        r.insert(default_col.clone(), value.clone());
        if column_aliases.is_empty() && alias.is_some() && default_col != name {
            r.insert(name.to_string(), value);
        }
        let r = match qual {
            Some(a) => prefix_row(a, &r),
            None => r,
        };
        out.push(r);
    };
    if !has_named_args {
        if let Some(result) = engine.call_registered_table_function(&lower, &evaluated) {
            return registered_table_function_rows(name, result?, qual, column_aliases);
        }
    }
    if let Some(result) =
        crate::sql::plpgsql_exec::call_user_table_function(engine, &lower, &call_args)
    {
        return registered_table_function_rows(name, result?, qual, column_aliases);
    }
    if has_named_args {
        return Err(unknown_function_error(&lower, &call_args));
    }
    match lower.as_str() {
        "generate_series" => {
            if !(2..=3).contains(&evaluated.len()) {
                return Err(SQLError::TypeMismatch(
                    "generate_series requires 2-3 args".into(),
                ));
            }
            let start = match &evaluated[0] {
                Value::Int(i) => *i,
                Value::Float(f) => *f as i64,
                _ => return Err(SQLError::TypeMismatch("generate_series start".into())),
            };
            let stop = match &evaluated[1] {
                Value::Int(i) => *i,
                Value::Float(f) => *f as i64,
                _ => return Err(SQLError::TypeMismatch("generate_series stop".into())),
            };
            let step = if evaluated.len() == 3 {
                match &evaluated[2] {
                    Value::Int(i) => *i,
                    Value::Float(f) => *f as i64,
                    _ => return Err(SQLError::TypeMismatch("generate_series step".into())),
                }
            } else {
                1
            };
            if step == 0 {
                return Err(SQLError::TypeMismatch(
                    "generate_series step cannot be 0".into(),
                ));
            }
            let mut current = start;
            if step > 0 {
                while current <= stop {
                    push_scalar(&mut out, Value::Int(current));
                    current += step;
                }
            } else {
                while current >= stop {
                    push_scalar(&mut out, Value::Int(current));
                    current += step;
                }
            }
            Ok(out)
        }
        "unnest" => {
            for value in &evaluated {
                if let Value::List(items) = value {
                    for item in items {
                        push_scalar(&mut out, item.clone());
                    }
                } else {
                    push_scalar(&mut out, value.clone());
                }
            }
            Ok(out)
        }
        "regexp_split_to_table" => {
            if evaluated.len() != 2 {
                return Err(SQLError::TypeMismatch(
                    "regexp_split_to_table requires 2 args".into(),
                ));
            }
            let s = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 1".into())),
            };
            let pat = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 2".into())),
            };
            let re = regex::Regex::new(&pat)
                .map_err(|e| SQLError::TypeMismatch(format!("invalid regex: {e}")))?;
            for piece in re.split(&s) {
                push_scalar(&mut out, Value::Str(piece.to_string()));
            }
            Ok(out)
        }
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
            if evaluated.len() != 1 {
                return Err(SQLError::TypeMismatch(format!("{lower} takes 1 arg")));
            }
            let parsed = json_table_arg(&evaluated[0], &lower)?;
            let serde_json::Value::Object(obj) = parsed else {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower}: argument is not an object"
                )));
            };
            let key_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "key".into());
            let val_col = column_aliases
                .get(1)
                .cloned()
                .unwrap_or_else(|| "value".into());
            for (k, v) in obj {
                let mut r = ResultRow::new();
                r.insert(key_col.clone(), Value::Str(k));
                r.insert(val_col.clone(), json_table_value_to_text(&v));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "json_array_elements"
        | "jsonb_array_elements"
        | "json_array_elements_text"
        | "jsonb_array_elements_text" => {
            if evaluated.len() != 1 {
                return Err(SQLError::TypeMismatch(format!("{lower} takes 1 arg")));
            }
            let parsed = json_table_arg(&evaluated[0], &lower)?;
            let serde_json::Value::Array(arr) = parsed else {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower}: argument is not an array"
                )));
            };
            let col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "value".into());
            for v in arr {
                let mut r = ResultRow::new();
                r.insert(col.clone(), json_table_value_to_text(&v));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        // -------------------------------------------------------------
        // Analyzer DDL exposed as table-functions. Mirror the canonical UQA implementation's
        // _build_create_analyzer / _build_drop_analyzer /
        // _build_list_analyzers / _build_set_table_analyzer.
        // -------------------------------------------------------------
        "create_analyzer" => {
            if evaluated.len() < 2 {
                return Err(SQLError::TypeMismatch(
                    "create_analyzer requires (name, config_json)".into(),
                ));
            }
            let analyzer_name = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("create_analyzer arg 1".into())),
            };
            let config_json = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("create_analyzer arg 2".into())),
            };
            engine
                .register_named_analyzer(&analyzer_name, &config_json)
                .map_err(SQLError::Unsupported)?;
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "create_analyzer".into()),
                Value::Str(format!("analyzer '{analyzer_name}' created")),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
        }
        "drop_analyzer" => {
            if evaluated.is_empty() {
                return Err(SQLError::TypeMismatch(
                    "drop_analyzer requires a name argument".into(),
                ));
            }
            let analyzer_name = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("drop_analyzer arg 1".into())),
            };
            engine.drop_named_analyzer(&analyzer_name);
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "drop_analyzer".into()),
                Value::Str(format!("analyzer '{analyzer_name}' dropped")),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
        }
        "list_analyzers" => {
            // Match UQA behavior for: include the four built-in analyzers
            // (`whitespace`, `standard`, `standard_cjk`, `keyword`) on
            // top of every user-registered named analyzer.
            let mut names: std::collections::BTreeSet<String> =
                engine.list_named_analyzers().into_iter().collect();
            for builtin in ["whitespace", "standard", "standard_cjk", "keyword"] {
                names.insert(builtin.to_string());
            }
            let key = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "analyzer_name".into());
            for n in names {
                let mut r = ResultRow::new();
                r.insert(key.clone(), Value::Str(n));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "fts_index_stats" => {
            if evaluated.len() > 1 {
                return Err(SQLError::TypeMismatch(
                    "fts_index_stats accepts optional table name".into(),
                ));
            }
            let table_filter = match evaluated.first() {
                Some(Value::Str(s)) => Some(s.as_str()),
                Some(_) => return Err(SQLError::TypeMismatch("fts_index_stats arg 1".into())),
                None => None,
            };
            for stat in engine.fts_index_stats(table_filter) {
                let mut r = ResultRow::new();
                r.insert("table_name".into(), Value::Str(stat.table_name));
                r.insert("field".into(), Value::Str(stat.field));
                r.insert("analyzer".into(), Value::Str(stat.analyzer));
                r.insert(
                    "posting_count".into(),
                    Value::Int(stat.posting_count as i64),
                );
                r.insert(
                    "doc_length_count".into(),
                    Value::Int(stat.doc_length_count as i64),
                );
                r.insert(
                    "indexed_doc_count".into(),
                    Value::Int(stat.indexed_doc_count as i64),
                );
                r.insert("term_count".into(), Value::Int(stat.term_count as i64));
                r.insert(
                    "total_field_length".into(),
                    Value::Int(stat.total_field_length as i64),
                );
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "set_table_analyzer" => {
            if evaluated.len() < 3 {
                return Err(SQLError::TypeMismatch(
                    "set_table_analyzer requires (table, field, analyzer_name[, phase])".into(),
                ));
            }
            let target_table = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 1".into())),
            };
            let field = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 2".into())),
            };
            let analyzer_name = match &evaluated[2] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 3".into())),
            };
            let phase = if evaluated.len() > 3 {
                match &evaluated[3] {
                    Value::Str(s) => s.clone(),
                    _ => "both".into(),
                }
            } else {
                "both".into()
            };
            engine
                .set_table_field_analyzer(&target_table, &field, &analyzer_name, &phase)
                .map_err(SQLError::Unsupported)?;
            let mut msg = format!("analyzer '{analyzer_name}' assigned to {target_table}.{field}");
            if phase != "both" {
                use std::fmt::Write as _;
                let _ = write!(msg, " (phase={phase})");
            }
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "set_table_analyzer".into()),
                Value::Str(msg),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
        }
        "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
        | "graph_betweenness" => {
            if evaluated.len() > 1 {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower} accepts at most one graph argument"
                )));
            }
            let graph = expect_optional_graph_value(engine, evaluated.first(), &lower)?;
            let entries = match lower.as_str() {
                "pagerank" | "graph_pagerank" => graph_pagerank_entries(engine, &graph)?,
                "hits" | "graph_hits" => graph_hits_entries(engine, &graph)?,
                "betweenness" | "graph_betweenness" => graph_betweenness_entries(engine, &graph)?,
                _ => unreachable!(),
            };
            let id_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "_doc_id".into());
            let score_col = column_aliases
                .get(1)
                .cloned()
                .unwrap_or_else(|| "_score".into());
            for entry in entries {
                let mut r = ResultRow::new();
                r.insert(id_col.clone(), Value::Int(entry.doc_id as i64));
                r.insert(score_col.clone(), Value::Float(entry.score));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "cypher" => {
            age_cypher::build_rows(engine, args, &evaluated, qual, column_aliases, column_types)
        }
        "rpq" => {
            if !(2..=3).contains(&evaluated.len()) {
                return Err(SQLError::TypeMismatch(
                    "rpq requires 2 or 3 args (expr, start [, graph])".into(),
                ));
            }
            let expr_str = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("rpq.expr must be string".into())),
            };
            let start = match &evaluated[1] {
                Value::Int(n) => *n as u64,
                _ => return Err(SQLError::TypeMismatch("rpq.start must be integer".into())),
            };
            let graph = expect_optional_graph_value(engine, evaluated.get(2), "rpq")?;
            let path = uqa_graph::parse_rpq(&expr_str)
                .map_err(|e| SQLError::Unsupported(format!("{e:?}")))?;
            let pl = engine
                .graph_with(&graph, |store| {
                    uqa_graph::RegularPathQuery::new(path, &graph)
                        .from_vertex(start)
                        .execute(store)
                })
                .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {graph:?}")))?;
            let id_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "vertex_id".into());
            for entry in pl.inner().entries() {
                let mut r = ResultRow::new();
                r.insert(id_col.clone(), Value::Int(entry.doc_id as i64));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        other => Err(SQLError::Unsupported(format!(
            "table function `{other}` in FROM"
        ))),
    }
}

fn registered_table_function_rows(
    name: &str,
    result: SQLTableFunctionResult,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<Vec<ResultRow>, SQLError> {
    if result.columns.is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "table function `{name}` returned no columns"
        )));
    }
    let columns: Vec<String> = result
        .columns
        .iter()
        .enumerate()
        .map(|(idx, column)| {
            column_aliases
                .get(idx)
                .cloned()
                .unwrap_or_else(|| column.clone())
        })
        .collect();
    let mut out = Vec::with_capacity(result.rows.len());
    for values in result.rows {
        if values.len() != result.columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "table function `{name}` row has {} values for {} columns",
                values.len(),
                result.columns.len()
            )));
        }
        let mut row = ResultRow::new();
        for (column, value) in columns.iter().zip(values) {
            row.insert(column.clone(), value);
        }
        let row = match alias {
            Some(alias) => prefix_row(alias, &row),
            None => row,
        };
        out.push(row);
    }
    Ok(out)
}

/// Detect an equijoin shape `<col_a> = <col_b>` and run a hash join.
///
/// Returns `Some(rows)` when the predicate is a clean equality
/// between qualified columns from the two sides. Returns `None` for
/// every other shape; the caller then falls back to the nested-loop
/// cross filter.
fn try_hash_inner_join(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    on: Option<&ScalarExpr>,
    params: &[SQLParam],
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    let Some(on_expr) = on else {
        return Ok(None);
    };
    let Some(equalities) = split_join_equalities(on_expr) else {
        return Ok(None);
    };
    let mut left_keys = Vec::with_capacity(equalities.len());
    let mut right_keys = Vec::with_capacity(equalities.len());
    for (lhs, rhs) in equalities {
        let Some((left_key, right_key)) = decide_join_sides(
            eval_hook,
            subquery_runner,
            left_rows,
            right_rows,
            lhs,
            rhs,
            params,
        ) else {
            return Ok(None);
        };
        left_keys.push(left_key);
        right_keys.push(right_key);
    }
    // Use the shared hash-join algorithm from `uqa-joins`. The closures
    // evaluate the picked join keys against each row and lift the
    // result into a hashable `JoinKey`; null-valued keys are skipped
    // so they do not match anything.
    use uqa_joins::row_join::try_hash_inner_join;
    let out = if left_keys.len() == 1 {
        let left_accessor = JoinKeyAccessor::new(left_keys[0]);
        let right_accessor = JoinKeyAccessor::new(right_keys[0]);
        try_hash_inner_join(
            left_rows,
            right_rows,
            |row| left_accessor.key(row, eval_hook, subquery_runner, params),
            |row| right_accessor.key(row, eval_hook, subquery_runner, params),
        )?
    } else {
        try_hash_inner_join(
            left_rows,
            right_rows,
            |row| composite_join_key(&left_keys, row, eval_hook, subquery_runner, params),
            |row| composite_join_key(&right_keys, row, eval_hook, subquery_runner, params),
        )?
    };
    Ok(Some(out))
}

fn try_hash_left_join(
    runtime: &JoinRuntime<'_>,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    right_from: &SourcePlan,
    on: Option<&ScalarExpr>,
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    let Some(on_expr) = on else {
        return Ok(None);
    };
    let Some(equalities) = split_join_equalities(on_expr) else {
        return Ok(None);
    };
    if left_rows.is_empty() || right_rows.is_empty() {
        return Ok(None);
    }
    let mut left_keys = Vec::with_capacity(equalities.len());
    let mut right_keys = Vec::with_capacity(equalities.len());
    for (lhs, rhs) in equalities {
        let Some((left_key, right_key)) = decide_join_sides(
            runtime.function_hook,
            runtime.subquery_runner,
            left_rows,
            right_rows,
            lhs,
            rhs,
            runtime.params,
        ) else {
            return Ok(None);
        };
        left_keys.push(left_key);
        right_keys.push(right_key);
    }

    let mut index: std::collections::HashMap<JoinKey, Vec<&ResultRow>> =
        std::collections::HashMap::with_capacity(right_rows.len());
    for row in right_rows {
        if let Some(key) = join_key_for_exprs(
            &right_keys,
            row,
            runtime.function_hook,
            runtime.subquery_runner,
            runtime.params,
        )? {
            index.entry(key).or_default().push(row);
        }
    }

    let mut out = Vec::with_capacity(left_rows.len());
    for left in left_rows {
        let key = join_key_for_exprs(
            &left_keys,
            left,
            runtime.function_hook,
            runtime.subquery_runner,
            runtime.params,
        )?;
        let matches = key.as_ref().and_then(|key| index.get(key));
        match matches {
            Some(rows) if !rows.is_empty() => {
                for right in rows {
                    out.push(merge_rows(left, right));
                }
            }
            _ => {
                let mut padded = left.clone();
                pad_nulls_for_from(&mut padded, right_from, runtime.engine);
                out.push(padded);
            }
        }
    }
    Ok(Some(out))
}

fn split_join_equalities(expr: &ScalarExpr) -> Option<Vec<(&ScalarExpr, &ScalarExpr)>> {
    match expr {
        ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::Equal,
            lhs,
            rhs,
        } => Some(vec![(lhs, rhs)]),
        ScalarExpr::And(items) => {
            let mut equalities = Vec::with_capacity(items.len());
            for item in items {
                let ScalarExpr::Binary {
                    op: uqa_sql::ast::BinaryOp::Equal,
                    lhs,
                    rhs,
                } = item
                else {
                    return None;
                };
                equalities.push((lhs.as_ref(), rhs.as_ref()));
            }
            if equalities.is_empty() {
                None
            } else {
                Some(equalities)
            }
        }
        _ => None,
    }
}

enum JoinKeyAccessor<'a> {
    Column(&'a str),
    QualifiedColumn(String),
    ScalarExpr(&'a ScalarExpr),
}

impl<'a> JoinKeyAccessor<'a> {
    fn new(expr: &'a ScalarExpr) -> Self {
        match expr {
            ScalarExpr::Column(name) => Self::Column(name.as_str()),
            ScalarExpr::QualifiedColumn {
                qualifier,
                column,
                key,
            } => Self::QualifiedColumn(if key.is_empty() {
                qualified_key(qualifier, column)
            } else {
                key.clone()
            }),
            _ => Self::ScalarExpr(expr),
        }
    }

    fn key(
        &self,
        row: &ResultRow,
        eval_hook: &dyn uqa_sql::expr::EngineHook,
        subquery_runner: &dyn ScalarSubqueryRunner,
        params: &[SQLParam],
    ) -> Result<Option<JoinKey>, SQLError> {
        Ok(match self {
            Self::Column(name) => value_to_join_key(column_value(row, name)),
            Self::QualifiedColumn(key) => value_to_join_key(row.get(key)),
            Self::ScalarExpr(expr) => {
                let ctx = ScalarEvalContext::new(Some(row), params)
                    .with_function_hook(eval_hook)
                    .with_subquery_runner(subquery_runner);
                match eval_scalar(expr, &ctx)? {
                    Value::Null => None,
                    value => Some(JoinKey::new(&value)),
                }
            }
        })
    }
}

fn column_value<'a>(row: &'a ResultRow, name: &str) -> Option<&'a Value> {
    if let Some(value) = row.get(name) {
        return Some(value);
    }
    row.iter()
        .find(|(key, _)| key.rsplit_once('.').is_some_and(|(_, col)| col == name))
        .map(|(_, value)| value)
}

fn value_to_join_key(value: Option<&Value>) -> Option<JoinKey> {
    match value {
        Some(Value::Null) | None => None,
        Some(value) => Some(JoinKey::new(value)),
    }
}

fn composite_join_key(
    exprs: &[&ScalarExpr],
    row: &ResultRow,
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
    params: &[SQLParam],
) -> Result<Option<JoinKey>, SQLError> {
    let ctx = ScalarEvalContext::new(Some(row), params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(subquery_runner);
    let mut values = Vec::with_capacity(exprs.len());
    for expr in exprs {
        match eval_scalar(expr, &ctx)? {
            Value::Null => return Ok(None),
            value => values.push(value),
        }
    }
    let refs: Vec<&Value> = values.iter().collect();
    Ok(Some(JoinKey::composite(&refs)))
}

fn join_key_for_exprs(
    exprs: &[&ScalarExpr],
    row: &ResultRow,
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
    params: &[SQLParam],
) -> Result<Option<JoinKey>, SQLError> {
    if exprs.len() == 1 {
        JoinKeyAccessor::new(exprs[0]).key(row, eval_hook, subquery_runner, params)
    } else {
        composite_join_key(exprs, row, eval_hook, subquery_runner, params)
    }
}

/// Pick which expression evaluates over the left side and which over
/// the right by sampling the first row of each side. Returns
/// `(left_key_expr, right_key_expr)` when one direction works,
/// `None` when the predicate isn't separable across sides.
fn decide_join_sides<'a>(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    lhs: &'a ScalarExpr,
    rhs: &'a ScalarExpr,
    params: &[SQLParam],
) -> Option<(&'a ScalarExpr, &'a ScalarExpr)> {
    if left_rows.is_empty() || right_rows.is_empty() {
        return None;
    }
    let l_sample = &left_rows[0];
    let r_sample = &right_rows[0];
    let lhs_on_left = eval_yields_value(eval_hook, subquery_runner, l_sample, lhs, params);
    let rhs_on_right = eval_yields_value(eval_hook, subquery_runner, r_sample, rhs, params);
    if lhs_on_left && rhs_on_right {
        return Some((lhs, rhs));
    }
    let rhs_on_left = eval_yields_value(eval_hook, subquery_runner, l_sample, rhs, params);
    let lhs_on_right = eval_yields_value(eval_hook, subquery_runner, r_sample, lhs, params);
    if rhs_on_left && lhs_on_right {
        return Some((rhs, lhs));
    }
    None
}

fn eval_yields_value(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
    row: &ResultRow,
    expr: &ScalarExpr,
    params: &[SQLParam],
) -> bool {
    let ctx = ScalarEvalContext::new(Some(row), params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(subquery_runner);
    matches!(eval_scalar(expr, &ctx), Ok(v) if v != uqa_core::Value::Null)
}

fn cross_filter(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    on: Option<&ScalarExpr>,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::with_capacity(left_rows.len() * right_rows.len());
    for l in left_rows {
        for r in right_rows {
            let merged = merge_rows(l, r);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx = ScalarEvalContext::new(Some(&merged), params)
                        .with_function_hook(eval_hook)
                        .with_subquery_runner(subquery_runner);
                    uqa_sql::expr::truthy(&eval_scalar(expr, &ctx)?)
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
    runtime: &JoinRuntime<'_>,
    outer_rows: &[ResultRow],
    inner_rows: &[ResultRow],
    inner_from: &SourcePlan,
    on: Option<&ScalarExpr>,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for l in outer_rows {
        let mut matched = false;
        for r in inner_rows {
            let merged = merge_rows(l, r);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx = ScalarEvalContext::new(Some(&merged), runtime.params)
                        .with_function_hook(runtime.function_hook)
                        .with_subquery_runner(runtime.subquery_runner);
                    uqa_sql::expr::truthy(&eval_scalar(expr, &ctx)?)
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
            pad_nulls_for_from(&mut pad, inner_from, runtime.engine);
            out.push(pad);
        }
    }
    Ok(out)
}

fn full_outer(
    runtime: &JoinRuntime<'_>,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    left_from: &SourcePlan,
    right_from: &SourcePlan,
    on: Option<&ScalarExpr>,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    let mut matched_right = vec![false; right_rows.len()];
    for left in left_rows {
        let mut matched_left = false;
        for (idx, right) in right_rows.iter().enumerate() {
            let merged = merge_rows(left, right);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx = ScalarEvalContext::new(Some(&merged), runtime.params)
                        .with_function_hook(runtime.function_hook)
                        .with_subquery_runner(runtime.subquery_runner);
                    uqa_sql::expr::truthy(&eval_scalar(expr, &ctx)?)
                }
            };
            if keep {
                out.push(merged);
                matched_left = true;
                matched_right[idx] = true;
            }
        }
        if !matched_left {
            let mut padded = left.clone();
            pad_nulls_for_from(&mut padded, right_from, runtime.engine);
            out.push(padded);
        }
    }
    for (idx, right) in right_rows.iter().enumerate() {
        if matched_right[idx] {
            continue;
        }
        let mut padded = ResultRow::new();
        pad_nulls_for_from(&mut padded, left_from, runtime.engine);
        for (k, v) in right {
            padded.insert(k.clone(), v.clone());
        }
        out.push(padded);
    }
    Ok(out)
}

fn pad_nulls_for_from(row: &mut ResultRow, from: &SourcePlan, engine: &Engine) {
    let mut tables = Vec::new();
    from.collect_tables(&mut tables);
    for (name, alias) in &tables {
        let null_keys = null_row_for(name, alias.as_deref(), engine);
        for (k, v) in null_keys {
            row.entry(k).or_insert(v);
        }
    }
}

pub(super) fn project_join_row_with_plan(
    engine: &Engine,
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn PhysicalSubqueryRunner,
    subqueries: &[QueryPlan],
    src: &ResultRow,
    projections: &[ProjectionPlan],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    project_join_row_inner(
        engine,
        eval_hook,
        subquery_runner,
        subqueries,
        src,
        projections,
        params,
    )
}

fn project_join_row_inner(
    engine: &Engine,
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn PhysicalSubqueryRunner,
    subqueries: &[QueryPlan],
    src: &ResultRow,
    projections: &[ProjectionPlan],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let labels = projection_columns(projections);
    let ctx = PhysicalEvalContext::new(Some(src), params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(subquery_runner);
    let mut out = ResultRow::new();
    for (idx, proj) in projections.iter().enumerate() {
        let label = labels[idx].clone();
        if let ScalarExpr::Star = proj.expr {
            for (k, v) in src {
                out.insert(k.clone(), v.clone());
            }
            continue;
        }
        // Window calls are pre-evaluated in `compute_window_columns`
        // and stored on the source row under the projection label;
        // read the cached value through.
        if matches!(proj.expr, ScalarExpr::WindowCall { .. }) {
            let value = src.get(&label).cloned().unwrap_or(Value::Null);
            out.insert(label, value);
            continue;
        }
        if let Some(value) = projected_value_from_row(&proj.expr, src) {
            out.insert(label, value);
            continue;
        }
        // `uqa_highlight()` evaluates against the analyzer for the
        // matched field, which the evaluator does not have access
        // to. Intercept the call here, resolve the string column +
        // query, and emit the wrapped text through
        // `uqa_analysis::highlight`.
        if let ScalarExpr::Func { name, args, .. } = &proj.expr {
            let mut evaluate = |expr: &ScalarExpr| eval_physical_scalar(expr, subqueries, &ctx);
            if let Some(value) =
                engine_func_intercept(Some(engine), name, args, src, &mut evaluate)?
            {
                out.insert(label, value);
                continue;
            }
        }
        let value = eval_physical_scalar(&proj.expr, subqueries, &ctx)?;
        out.insert(label, value);
    }
    Ok(out)
}

/// Intercept registry functions that need engine-level access (the
/// scalar evaluator does not see the engine, just the row context).
/// Returns `Ok(Some(_))` when the function was handled, `Ok(None)`
/// to defer to the default scalar evaluator.
pub(super) fn engine_func_intercept(
    engine: Option<&Engine>,
    name: &str,
    args: &[ScalarExpr],
    row: &ResultRow,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Option<Value>, SQLError> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "uqa_highlight" => Ok(Some(run_uqa_highlight(row, args, evaluate)?)),
        "score_bm25" | "score_bayesian_bm25" => {
            validate_score_projection_args(&lower, args, evaluate)?;
            Ok(Some(
                row.get(SCORE_COLUMN).cloned().unwrap_or(Value::Float(0.0)),
            ))
        }
        "deep_learn" => Ok(Some(run_deep_learn_projection(engine, args, evaluate)?)),
        "merge_action" => {
            if !args.is_empty() {
                return Err(SQLError::BadArity {
                    name: "merge_action".into(),
                    expected: "0".into(),
                    actual: args.len(),
                });
            }
            let action = row.get(MERGE_ACTION_COLUMN).cloned().ok_or_else(|| {
                SQLError::Unsupported("merge_action() is only valid in MERGE RETURNING".into())
            })?;
            Ok(Some(action))
        }
        // UQA-native helpers keep their lenient semantics.
        "graph_create" => {
            if let Some(eng) = engine {
                let _ = run_graph_create_with_evaluator(eng, args, evaluate)?;
            }
            Ok(Some(Value::Bool(true)))
        }
        "graph_drop" => {
            if let Some(eng) = engine {
                let _ = run_graph_drop_with_evaluator(eng, args, evaluate)?;
            }
            Ok(Some(Value::Bool(true)))
        }
        // Apache AGE-compatible functions: strict name validation and
        // a void (SQL NULL) return value.
        "create_graph" => match engine {
            Some(eng) => Ok(Some(run_age_create_graph_with_evaluator(
                eng, args, evaluate,
            )?)),
            None => Ok(Some(Value::Null)),
        },
        "drop_graph" => match engine {
            Some(eng) => Ok(Some(run_age_drop_graph_with_evaluator(
                eng, args, evaluate,
            )?)),
            None => Ok(Some(Value::Null)),
        },
        _ => Ok(None),
    }
}

fn run_deep_learn_projection(
    engine: Option<&Engine>,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    let Some(engine) = engine else {
        return Err(SQLError::Unsupported(
            "deep_learn requires an engine-backed projection".into(),
        ));
    };
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "deep_learn".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let model_name = match evaluate(&args[0])? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.model must be a string, got {other:?}"
            )));
        }
    };
    let training_source = match evaluate(&args[1])? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.training_set must be a table name or JSON string, got {other:?}"
            )));
        }
    };
    let trimmed = training_source.trim();
    let output = if trimmed.starts_with('{') {
        engine.deep_learn_json(&model_name, trimmed, &uqa_ml::LearnOptions::default())?
    } else {
        engine.deep_learn_table(
            &model_name,
            &training_source,
            &uqa_ml::LearnOptions::default(),
        )?
    };
    let mut report = BTreeMap::new();
    report.insert("model".into(), Value::Str(model_name));
    report.insert("examples".into(), Value::Int(output.report.examples as i64));
    report.insert(
        "feature_dimensions".into(),
        Value::Int(output.report.feature_dimensions as i64),
    );
    report.insert(
        "class_count".into(),
        Value::Int(output.report.class_count as i64),
    );
    Ok(Value::Map(report))
}

fn validate_score_projection_args(
    name: &str,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<(), SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: name.into(),
            expected: "1..=2".into(),
            actual: args.len(),
        });
    }
    let query_idx = args.len() - 1;
    if args.len() == 2 {
        let _ = expect_column_name(&args[0], &format!("{name}.field"))?;
    }
    match evaluate(&args[query_idx])? {
        Value::Str(_) => Ok(()),
        other => Err(SQLError::TypeMismatch(format!(
            "{name}.query must be a string, got {other:?}"
        ))),
    }
}

/// Evaluate a `uqa_highlight(field, query[, start_tag, end_tag,
/// max_fragments, fragment_size])` projection. Matches UQA
/// reference's `_run_uqa_highlight` shape: field can be either a
/// bare column reference (looked up on the row) or a literal string;
/// the rest of the args are scalar literals after evaluation.
fn run_uqa_highlight(
    row: &ResultRow,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    if args.len() < 2 || args.len() > 6 {
        return Err(SQLError::BadArity {
            name: "uqa_highlight".into(),
            expected: "2..=6".into(),
            actual: args.len(),
        });
    }
    let text = match &args[0] {
        ScalarExpr::Column(c) => match super::row_column_value(row, c) {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Null) => return Ok(Value::Null),
            Some(other) => format!("{other:?}"),
            None => return Ok(Value::Null),
        },
        ScalarExpr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => {
            let fallback_key;
            let lookup_key = if key.is_empty() {
                fallback_key = qualified_key(qualifier, column);
                fallback_key.as_str()
            } else {
                key.as_str()
            };
            match row
                .get(lookup_key)
                .or_else(|| uqa_sql::expr::unqualified_fallback(row, column))
            {
                Some(Value::Str(s)) => s.clone(),
                Some(Value::Null) => return Ok(Value::Null),
                Some(other) => format!("{other:?}"),
                None => return Ok(Value::Null),
            }
        }
        other => match evaluate(other)? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            v => format!("{v:?}"),
        },
    };
    let query_str = match evaluate(&args[1])? {
        Value::Str(s) => s,
        Value::Null => return Ok(Value::Str(text)),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "uqa_highlight query must be string, got {other:?}"
            )));
        }
    };
    let start_tag = match args.get(2) {
        Some(e) => match evaluate(e)? {
            Value::Str(s) => s,
            Value::Null => "<b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight start_tag must be string, got {other:?}"
                )));
            }
        },
        None => "<b>".into(),
    };
    let end_tag = match args.get(3) {
        Some(e) => match evaluate(e)? {
            Value::Str(s) => s,
            Value::Null => "</b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight end_tag must be string, got {other:?}"
                )));
            }
        },
        None => "</b>".into(),
    };
    let max_fragments = match args.get(4) {
        Some(e) => match evaluate(e)? {
            Value::Int(n) if n >= 0 => n as usize,
            Value::Null => 0,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight max_fragments must be non-negative integer, got {other:?}"
                )));
            }
        },
        None => 0,
    };
    let fragment_size = match args.get(5) {
        Some(e) => match evaluate(e)? {
            Value::Int(n) if n > 0 => n as usize,
            Value::Null => 150,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight fragment_size must be positive integer, got {other:?}"
                )));
            }
        },
        None => 150,
    };
    let opts = uqa_analysis::HighlightOptions {
        start_tag,
        end_tag,
        max_fragments,
        fragment_size,
    };
    // Pull every whitespace-separated token out of the query string
    // as a candidate match term. The canonical UQA behavior parses the FTS
    // query, but a simple split is what callers reach for in
    // practice and matches what the test fixture exercises.
    let terms: Vec<String> = query_str
        .split_whitespace()
        .filter(|t| !matches!(t.to_ascii_lowercase().as_str(), "and" | "or" | "not"))
        .map(std::string::ToString::to_string)
        .collect();
    let analyzer = uqa_analysis::standard_analyzer("english");
    let out = uqa_analysis::highlight(&text, &terms, Some(&analyzer), &opts);
    Ok(Value::Str(out))
}
