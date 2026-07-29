//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM/JOIN row assembly, table functions, and projection intercepts.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::Value;
use uqa_joins::row_join::JoinKey;
use uqa_sql::ast::{Expr, FromClause, JoinKind, Projection, SelectStmt, SetOpKind};
use uqa_sql::expr::{eval, EvalContext};
use uqa_sql::{ResultRow, SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::Document;

use crate::{Engine, SQLTableFunctionResult};

use super::select::{
    execute_query_plan_with_ctes, execute_select_ast, execute_select_ast_with_ctes,
    select_contains_volatile_function, CteScope, ScopedEngineHook,
};
use super::{
    age_cypher, aggregate_join_rows, apply_row_order_limit, build_info_schema_rows,
    expect_column_name, expect_optional_graph_value, graph_betweenness_entries, graph_hits_entries,
    graph_pagerank_entries, has_aggregate, json_table_arg, json_table_value_to_text,
    materialize_ctes, projected_value_from_row, projection_columns, run_age_create_graph,
    run_age_drop_graph, run_graph_create, run_graph_drop, MERGE_ACTION_COLUMN, SCORE_COLUMN,
};

pub(super) type ColumnPrune = BTreeMap<String, BTreeSet<String>>;
pub(super) type QualifierFilters = BTreeMap<String, Vec<Expr>>;
type RowFilter<'a> = &'a mut dyn FnMut(&mut Vec<ResultRow>) -> Result<(), SQLError>;

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
        let ctx = EvalContext::new(Some(&row), params).with_engine(engine);
        if uqa_sql::expr::truthy(&eval(&filter, &ctx)?) {
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

    let mut projections = vec![Projection {
        expr: Expr::Star,
        alias: None,
    }];
    if prune
        .and_then(|prune| prune.get(qual))
        .is_some_and(|columns| columns.contains(SCORE_COLUMN))
    {
        projections.push(Projection {
            expr: Expr::Column(SCORE_COLUMN.to_string()),
            alias: None,
        });
    }

    let stmt = SelectStmt {
        projections,
        from: Some(FromClause::Table {
            name: table.to_string(),
            alias: None,
        }),
        r#where: Some(filter),
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        having: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        with: Vec::new(),
        set_op: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let result = execute_select_ast(engine, &stmt, params)?;
    Ok(Some(reprefix_rows_pruned(qual, &result.rows, prune)))
}

fn dequalify_expr_for_qualifier(expr: &Expr, qual: &str) -> Option<Expr> {
    match expr {
        Expr::QualifiedColumn {
            qualifier, column, ..
        } => (qualifier == qual).then(|| Expr::Column(column.clone())),
        Expr::Column(_) | Expr::Literal(_) | Expr::Param(_) | Expr::Star => Some(expr.clone()),
        Expr::Array(items) => Some(Expr::Array(
            items
                .iter()
                .map(|item| dequalify_expr_for_qualifier(item, qual))
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::And(items) => Some(Expr::And(
            items
                .iter()
                .map(|item| dequalify_expr_for_qualifier(item, qual))
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::Or(items) => Some(Expr::Or(
            items
                .iter()
                .map(|item| dequalify_expr_for_qualifier(item, qual))
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::Binary { op, lhs, rhs } => Some(Expr::Binary {
            op: *op,
            lhs: Box::new(dequalify_expr_for_qualifier(lhs, qual)?),
            rhs: Box::new(dequalify_expr_for_qualifier(rhs, qual)?),
        }),
        Expr::Not(inner) => Some(Expr::Not(Box::new(dequalify_expr_for_qualifier(
            inner, qual,
        )?))),
        Expr::IsNull { expr, negated } => Some(Expr::IsNull {
            expr: Box::new(dequalify_expr_for_qualifier(expr, qual)?),
            negated: *negated,
        }),
        Expr::Between { expr, low, high } => Some(Expr::Between {
            expr: Box::new(dequalify_expr_for_qualifier(expr, qual)?),
            low: Box::new(dequalify_expr_for_qualifier(low, qual)?),
            high: Box::new(dequalify_expr_for_qualifier(high, qual)?),
        }),
        Expr::InList {
            expr,
            list,
            negated,
        } => Some(Expr::InList {
            expr: Box::new(dequalify_expr_for_qualifier(expr, qual)?),
            list: list
                .iter()
                .map(|item| dequalify_expr_for_qualifier(item, qual))
                .collect::<Option<Vec<_>>>()?,
            negated: *negated,
        }),
        Expr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => Some(Expr::Func {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| dequalify_expr_for_qualifier(arg, qual))
                .collect::<Option<Vec<_>>>()?,
            distinct: *distinct,
            order_by: order_by
                .iter()
                .map(|order| {
                    Some(uqa_sql::ast::OrderBy {
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
        Expr::WindowCall { name, args, spec } => {
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
                    Some(uqa_sql::ast::OrderBy {
                        expr: dequalify_expr_for_qualifier(&order.expr, qual)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Expr::WindowCall {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| dequalify_expr_for_qualifier(arg, qual))
                    .collect::<Option<Vec<_>>>()?,
                spec,
            })
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => Some(Expr::Case {
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
        Expr::Cast { expr, ty } => Some(Expr::Cast {
            expr: Box::new(dequalify_expr_for_qualifier(expr, qual)?),
            ty: ty.clone(),
        }),
        Expr::InSubquery { .. } | Expr::ScalarSubquery(_) | Expr::Exists { .. } => None,
    }
}

fn combine_filters(filters: impl IntoIterator<Item = Expr>) -> Expr {
    let mut filters: Vec<Expr> = filters.into_iter().collect();
    if filters.len() == 1 {
        filters.pop().unwrap()
    } else {
        Expr::And(filters)
    }
}

fn specialize_select_for_qualifier_filters(
    engine: &Engine,
    body: &SelectStmt,
    filters: Option<&QualifierFilters>,
    qual: &str,
) -> Option<SelectStmt> {
    let filters = filters?.get(qual)?;
    if filters.is_empty() {
        return None;
    }
    if select_contains_volatile_function(body) {
        return None;
    }
    let filter = combine_filters(filters.iter().cloned());
    push_output_filter_into_select(engine, body.clone(), qual, &filter)
}

pub(super) fn push_output_filter_into_select(
    engine: &Engine,
    stmt: SelectStmt,
    qual: &str,
    filter: &Expr,
) -> Option<SelectStmt> {
    push_output_filter_into_select_inner(engine, stmt, qual, filter, None)
}

pub(super) fn push_output_filter_into_select_with_columns(
    engine: &Engine,
    stmt: SelectStmt,
    qual: &str,
    filter: &Expr,
    output_columns: &[String],
) -> Option<SelectStmt> {
    push_output_filter_into_select_inner(engine, stmt, qual, filter, Some(output_columns))
}

fn push_output_filter_into_select_inner(
    engine: &Engine,
    mut stmt: SelectStmt,
    qual: &str,
    filter: &Expr,
    output_columns: Option<&[String]>,
) -> Option<SelectStmt> {
    if let Some(mut set_op) = stmt.set_op.take() {
        if set_op.kind != SetOpKind::Union
            || !set_op.all
            || !set_op.combined_order_by.is_empty()
            || set_op.combined_limit.is_some()
            || set_op.combined_offset.is_some()
        {
            stmt.set_op = Some(set_op);
            return None;
        }
        let right = push_output_filter_into_select_inner(
            engine,
            set_op.right,
            qual,
            filter,
            output_columns,
        )?;
        if let Some(left) = set_op.left.take() {
            let left =
                push_output_filter_into_select_inner(engine, *left, qual, filter, output_columns)?;
            set_op.left = Some(Box::new(left));
            set_op.right = right;
            stmt.set_op = Some(set_op);
            return Some(stmt);
        }
        let lhs = push_output_filter_into_select_inner(engine, stmt, qual, filter, output_columns)?;
        let mut out = lhs;
        set_op.right = right;
        out.set_op = Some(set_op);
        return Some(out);
    }

    if stmt.limit.is_some()
        || stmt.offset.is_some()
        || stmt.distinct
        || !stmt.distinct_on.is_empty()
    {
        return None;
    }
    if has_window_projection_in_select(&stmt) {
        return None;
    }
    let rewritten = rewrite_output_filter_for_select(filter, qual, &stmt, output_columns)?;
    if !filter_can_apply_before_group(engine, &stmt, &rewritten) {
        return None;
    }
    stmt.r#where = Some(match stmt.r#where.take() {
        Some(existing) => Expr::And(vec![existing, rewritten]),
        None => rewritten,
    });
    Some(stmt)
}

fn rewrite_output_filter_for_select(
    filter: &Expr,
    qual: &str,
    stmt: &SelectStmt,
    output_columns: Option<&[String]>,
) -> Option<Expr> {
    let output_map = direct_projection_output_map(stmt, output_columns)?;
    let star_qualifier = star_projection_source_qualifier(stmt);
    rewrite_output_filter_expr(filter, qual, &output_map, star_qualifier.as_deref())
}

fn direct_projection_output_map(
    stmt: &SelectStmt,
    output_columns: Option<&[String]>,
) -> Option<BTreeMap<String, Expr>> {
    let owned_labels;
    let labels: &[String] = if let Some(columns) = output_columns {
        if columns.len() != stmt.projections.len() {
            return None;
        }
        columns
    } else {
        owned_labels = projection_columns(&stmt.projections);
        &owned_labels
    };
    let mut out = BTreeMap::new();
    for (idx, projection) in stmt.projections.iter().enumerate() {
        if matches!(projection.expr, Expr::Star) {
            continue;
        }
        if expr_contains_subquery_local(&projection.expr)
            || expr_contains_volatile_local(&projection.expr)
            || expr_has_window(&projection.expr)
        {
            continue;
        }
        if direct_column_name(&projection.expr).is_some() {
            out.insert(labels[idx].clone(), projection.expr.clone());
        }
    }
    Some(out)
}

fn star_projection_source_qualifier(stmt: &SelectStmt) -> Option<String> {
    if stmt.projections.len() != 1 || !matches!(stmt.projections[0].expr, Expr::Star) {
        return None;
    }
    match stmt.from.as_ref()? {
        FromClause::Table { name, alias } => Some(alias.clone().unwrap_or_else(|| name.clone())),
        _ => None,
    }
}

fn rewrite_output_filter_expr(
    expr: &Expr,
    qual: &str,
    output_map: &BTreeMap<String, Expr>,
    star_qualifier: Option<&str>,
) -> Option<Expr> {
    match expr {
        Expr::QualifiedColumn {
            qualifier, column, ..
        } => {
            if qualifier == qual {
                output_map
                    .get(column)
                    .cloned()
                    .or_else(|| star_qualifier.map(|source| Expr::qualified_column(source, column)))
            } else {
                None
            }
        }
        Expr::Column(column) => output_map
            .get(column)
            .cloned()
            .or_else(|| star_qualifier.map(|source| Expr::qualified_column(source, column))),
        Expr::Literal(value) => Some(Expr::Literal(value.clone())),
        Expr::Param(index) => Some(Expr::Param(*index)),
        Expr::Array(items) => Some(Expr::Array(
            items
                .iter()
                .map(|item| rewrite_output_filter_expr(item, qual, output_map, star_qualifier))
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::And(items) => Some(Expr::And(
            items
                .iter()
                .map(|item| rewrite_output_filter_expr(item, qual, output_map, star_qualifier))
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::Or(items) => Some(Expr::Or(
            items
                .iter()
                .map(|item| rewrite_output_filter_expr(item, qual, output_map, star_qualifier))
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::Binary { op, lhs, rhs } => Some(Expr::Binary {
            op: *op,
            lhs: Box::new(rewrite_output_filter_expr(
                lhs,
                qual,
                output_map,
                star_qualifier,
            )?),
            rhs: Box::new(rewrite_output_filter_expr(
                rhs,
                qual,
                output_map,
                star_qualifier,
            )?),
        }),
        Expr::Not(inner) => Some(Expr::Not(Box::new(rewrite_output_filter_expr(
            inner,
            qual,
            output_map,
            star_qualifier,
        )?))),
        Expr::IsNull { expr, negated } => Some(Expr::IsNull {
            expr: Box::new(rewrite_output_filter_expr(
                expr,
                qual,
                output_map,
                star_qualifier,
            )?),
            negated: *negated,
        }),
        Expr::Between { expr, low, high } => Some(Expr::Between {
            expr: Box::new(rewrite_output_filter_expr(
                expr,
                qual,
                output_map,
                star_qualifier,
            )?),
            low: Box::new(rewrite_output_filter_expr(
                low,
                qual,
                output_map,
                star_qualifier,
            )?),
            high: Box::new(rewrite_output_filter_expr(
                high,
                qual,
                output_map,
                star_qualifier,
            )?),
        }),
        Expr::InList {
            expr,
            list,
            negated,
        } => Some(Expr::InList {
            expr: Box::new(rewrite_output_filter_expr(
                expr,
                qual,
                output_map,
                star_qualifier,
            )?),
            list: list
                .iter()
                .map(|item| rewrite_output_filter_expr(item, qual, output_map, star_qualifier))
                .collect::<Option<Vec<_>>>()?,
            negated: *negated,
        }),
        Expr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => Some(Expr::Func {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| rewrite_output_filter_expr(arg, qual, output_map, star_qualifier))
                .collect::<Option<Vec<_>>>()?,
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: match filter.as_ref() {
                Some(filter) => Some(Box::new(rewrite_output_filter_expr(
                    filter,
                    qual,
                    output_map,
                    star_qualifier,
                )?)),
                None => None,
            },
        }),
        Expr::Cast { expr, ty } => Some(Expr::Cast {
            expr: Box::new(rewrite_output_filter_expr(
                expr,
                qual,
                output_map,
                star_qualifier,
            )?),
            ty: ty.clone(),
        }),
        Expr::Case {
            base,
            when,
            else_branch,
        } => Some(Expr::Case {
            base: match base.as_ref() {
                Some(expr) => Some(Box::new(rewrite_output_filter_expr(
                    expr,
                    qual,
                    output_map,
                    star_qualifier,
                )?)),
                None => None,
            },
            when: when
                .iter()
                .map(|(cond, result)| {
                    Some((
                        rewrite_output_filter_expr(cond, qual, output_map, star_qualifier)?,
                        rewrite_output_filter_expr(result, qual, output_map, star_qualifier)?,
                    ))
                })
                .collect::<Option<Vec<_>>>()?,
            else_branch: match else_branch.as_ref() {
                Some(expr) => Some(Box::new(rewrite_output_filter_expr(
                    expr,
                    qual,
                    output_map,
                    star_qualifier,
                )?)),
                None => None,
            },
        }),
        Expr::Star
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => None,
    }
}

fn filter_can_apply_before_group(engine: &Engine, stmt: &SelectStmt, filter: &Expr) -> bool {
    if stmt.grouping_sets.is_empty()
        && stmt.group_by.is_empty()
        && !has_aggregate(engine, &stmt.projections)
        && stmt.having.is_none()
    {
        return true;
    }
    if !stmt.grouping_sets.is_empty() || stmt.having.is_some() {
        return false;
    }
    let group_columns: BTreeSet<String> = stmt
        .group_by
        .iter()
        .filter_map(direct_column_name)
        .collect();
    if group_columns.is_empty() {
        return false;
    }
    filter_column_names(filter)
        .into_iter()
        .all(|column| group_columns.contains(&column))
}

fn direct_column_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Column(column) => Some(column.clone()),
        Expr::QualifiedColumn { column, .. } => Some(column.clone()),
        _ => None,
    }
}

fn filter_column_names(expr: &Expr) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_filter_column_names(expr, &mut out);
    out
}

fn collect_filter_column_names(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Column(column) | Expr::QualifiedColumn { column, .. } => {
            out.insert(column.clone());
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_filter_column_names(item, out);
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for arg in args {
                collect_filter_column_names(arg, out);
            }
            for order in order_by {
                collect_filter_column_names(&order.expr, out);
            }
            if let Some(filter) = filter.as_ref() {
                collect_filter_column_names(filter, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_filter_column_names(lhs, out);
            collect_filter_column_names(rhs, out);
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            collect_filter_column_names(inner, out);
        }
        Expr::Between { expr, low, high } => {
            collect_filter_column_names(expr, out);
            collect_filter_column_names(low, out);
            collect_filter_column_names(high, out);
        }
        Expr::InList { expr, list, .. } => {
            collect_filter_column_names(expr, out);
            for item in list {
                collect_filter_column_names(item, out);
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_ref() {
                collect_filter_column_names(base, out);
            }
            for (cond, result) in when {
                collect_filter_column_names(cond, out);
                collect_filter_column_names(result, out);
            }
            if let Some(else_branch) = else_branch.as_ref() {
                collect_filter_column_names(else_branch, out);
            }
        }
        Expr::Literal(_)
        | Expr::Param(_)
        | Expr::Star
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => {}
    }
}

fn has_window_projection_in_select(stmt: &SelectStmt) -> bool {
    stmt.projections
        .iter()
        .any(|projection| expr_has_window(&projection.expr))
        || stmt.group_by.iter().any(expr_has_window)
        || stmt
            .grouping_sets
            .iter()
            .any(|set| set.iter().any(expr_has_window))
        || stmt.having.as_ref().is_some_and(expr_has_window)
        || stmt
            .order_by
            .iter()
            .any(|order| expr_has_window(&order.expr))
}

fn expr_has_window(expr: &Expr) -> bool {
    match expr {
        Expr::WindowCall { .. } => true,
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().any(expr_has_window)
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_has_window)
                || order_by.iter().any(|order| expr_has_window(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|filter| expr_has_window(filter))
        }
        Expr::Binary { lhs, rhs, .. } => expr_has_window(lhs) || expr_has_window(rhs),
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expr_has_window(inner)
        }
        Expr::Between { expr, low, high } => {
            expr_has_window(expr) || expr_has_window(low) || expr_has_window(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_has_window(expr) || list.iter().any(expr_has_window)
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref().is_some_and(|expr| expr_has_window(expr))
                || when
                    .iter()
                    .any(|(cond, result)| expr_has_window(cond) || expr_has_window(result))
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_has_window(expr))
        }
        Expr::InSubquery { expr, .. } => expr_has_window(expr),
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_)
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => false,
    }
}

fn expr_contains_subquery_local(expr: &Expr) -> bool {
    match expr {
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => true,
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().any(expr_contains_subquery_local)
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_contains_subquery_local)
                || order_by
                    .iter()
                    .any(|order| expr_contains_subquery_local(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|filter| expr_contains_subquery_local(filter))
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_contains_subquery_local(lhs) || expr_contains_subquery_local(rhs)
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expr_contains_subquery_local(inner)
        }
        Expr::Between { expr, low, high } => {
            expr_contains_subquery_local(expr)
                || expr_contains_subquery_local(low)
                || expr_contains_subquery_local(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_contains_subquery_local(expr) || list.iter().any(expr_contains_subquery_local)
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| expr_contains_subquery_local(expr))
                || when.iter().any(|(cond, result)| {
                    expr_contains_subquery_local(cond) || expr_contains_subquery_local(result)
                })
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_contains_subquery_local(expr))
        }
        Expr::WindowCall { args, spec, .. } => {
            args.iter().any(expr_contains_subquery_local)
                || spec.partition_by.iter().any(expr_contains_subquery_local)
                || spec
                    .order_by
                    .iter()
                    .any(|order| expr_contains_subquery_local(&order.expr))
        }
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => false,
    }
}

fn expr_contains_volatile_local(expr: &Expr) -> bool {
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
            ) || args.iter().any(expr_contains_volatile_local)
                || order_by
                    .iter()
                    .any(|order| expr_contains_volatile_local(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|filter| expr_contains_volatile_local(filter))
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().any(expr_contains_volatile_local)
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_contains_volatile_local(lhs) || expr_contains_volatile_local(rhs)
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expr_contains_volatile_local(inner)
        }
        Expr::Between { expr, low, high } => {
            expr_contains_volatile_local(expr)
                || expr_contains_volatile_local(low)
                || expr_contains_volatile_local(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_contains_volatile_local(expr) || list.iter().any(expr_contains_volatile_local)
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| expr_contains_volatile_local(expr))
                || when.iter().any(|(cond, result)| {
                    expr_contains_volatile_local(cond) || expr_contains_volatile_local(result)
                })
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_contains_volatile_local(expr))
        }
        Expr::WindowCall { args, spec, .. } => {
            args.iter().any(expr_contains_volatile_local)
                || spec.partition_by.iter().any(expr_contains_volatile_local)
                || spec
                    .order_by
                    .iter()
                    .any(|order| expr_contains_volatile_local(&order.expr))
        }
        Expr::InSubquery { expr, body, .. } => {
            expr_contains_volatile_local(expr) || select_contains_volatile_function(body)
        }
        Expr::ScalarSubquery(body) | Expr::Exists { body, .. } => {
            select_contains_volatile_function(body)
        }
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => false,
    }
}

fn propagated_join_filters(
    filters: &QualifierFilters,
    source_from: &FromClause,
    target_from: &FromClause,
    on: Option<&Expr>,
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
                let propagated = Expr::Binary {
                    op: uqa_sql::ast::BinaryOp::Equal,
                    lhs: Box::new(Expr::qualified_column(&target.0, &target.1)),
                    rhs: Box::new(value),
                };
                out.entry(target.0.clone()).or_default().push(propagated);
                changed = true;
            }
        }
    }
    changed
}

fn constant_equality_for_column(expr: &Expr, qual: &str, column: &str) -> Option<Expr> {
    let Expr::Binary {
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

fn expr_is_qualified_column(expr: &Expr, qual: &str, column: &str) -> bool {
    matches!(
        expr,
        Expr::QualifiedColumn {
            qualifier,
            column: col,
            ..
        } if qualifier == qual && col == column
    )
}

fn expr_is_constant(expr: &Expr) -> bool {
    matches!(expr, Expr::Literal(_) | Expr::Param(_))
}

fn join_column_equalities(expr: &Expr) -> Vec<((String, String), (String, String))> {
    match expr {
        Expr::And(items) => items.iter().flat_map(join_column_equalities).collect(),
        Expr::Binary {
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

fn qualified_column_pair(expr: &Expr) -> Option<(String, String)> {
    match expr {
        Expr::QualifiedColumn {
            qualifier, column, ..
        } => Some((qualifier.clone(), column.clone())),
        _ => None,
    }
}

fn from_qualifiers(from: &FromClause) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_from_qualifiers(from, &mut out);
    out
}

fn collect_from_qualifiers(from: &FromClause, out: &mut BTreeSet<String>) {
    match from {
        FromClause::Table { name, alias } => {
            out.insert(alias.clone().unwrap_or_else(|| name.clone()));
        }
        FromClause::Join { left, right, .. } => {
            collect_from_qualifiers(left, out);
            collect_from_qualifiers(right, out);
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
        out.insert(qualified_key(&qual, &k), Value::Null);
    }
    out
}

pub(super) fn build_join_rows_with_ctes(
    engine: &Engine,
    from: &FromClause,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut row_filter = None;
    build_join_rows_with_ctes_inner(engine, from, params, ctes, &mut row_filter, None, None)
}

pub(super) fn build_join_rows_with_ctes_pruned(
    engine: &Engine,
    from: &FromClause,
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
    from: &FromClause,
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
    from: &FromClause,
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
    from: &FromClause,
    params: &[SQLParam],
    ctes: &mut CteScope,
    row_filter: RowFilter<'_>,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut row_filter = Some(row_filter);
    build_join_rows_with_ctes_inner(engine, from, params, ctes, &mut row_filter, None, None)
}

pub(super) fn build_join_rows_with_ctes_filtered_pruned(
    engine: &Engine,
    from: &FromClause,
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
    from: &FromClause,
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
    from: &FromClause,
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
    from: &FromClause,
    params: &[SQLParam],
    ctes: &mut CteScope,
    row_filter: &mut Option<RowFilter<'_>>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Vec<ResultRow>, SQLError> {
    match from {
        FromClause::Table { name, alias } => {
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
            if let Some(body) = ctes.inlined.get(name).cloned() {
                let local_cte_names = select_cte_names(&body);
                let specialized =
                    specialize_select_for_qualifier_filters(engine, &body, filters, &qual);
                let is_specialized = specialized.is_some();
                let body = specialized.unwrap_or(body);
                let result =
                    execute_view_with_parent_cache(engine, &body, params, ctes, &local_cte_names)?;
                let prefixed = reprefix_rows_pruned(&qual, &result.rows, prune);
                let prefixed = apply_qualifier_filters(engine, prefixed, filters, &qual, params)?;
                if prune.is_none() && !is_specialized && !has_filters_for_qualifier(filters, &qual)
                {
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
            if let Some(body) = engine.view(name) {
                let plan = engine.view_plan(name).ok_or_else(|| {
                    SQLError::Internal(format!("view `{name}` has no compiled QueryPlan"))
                })?;
                let local_cte_names = select_cte_names(&body);
                let is_volatile = select_contains_volatile_function(&body);
                let specialized =
                    specialize_select_for_qualifier_filters(engine, &body, filters, &qual);
                let is_specialized = specialized.is_some();
                let result = if let Some(specialized) = specialized {
                    if is_volatile {
                        let mut scoped_ctes = ctes.clone();
                        execute_select_ast_with_ctes(
                            engine,
                            &specialized,
                            params,
                            &mut scoped_ctes,
                        )?
                    } else {
                        execute_view_with_parent_cache(
                            engine,
                            &specialized,
                            params,
                            ctes,
                            &local_cte_names,
                        )?
                    }
                } else if is_volatile {
                    let mut scoped_ctes = ctes.clone();
                    execute_query_plan_with_ctes(engine, &plan, params, &mut scoped_ctes)?
                } else {
                    execute_view_plan_with_parent_cache(
                        engine,
                        &plan,
                        params,
                        ctes,
                        &local_cte_names,
                    )?
                };
                let prefixed = reprefix_rows_pruned(&qual, &result.rows, prune);
                let prefixed = apply_qualifier_filters(engine, prefixed, filters, &qual, params)?;
                if !is_volatile && !is_specialized {
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
        FromClause::Join {
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
            // engine substitutes the outer row into the EvalContext
            // through the row-level evaluator.
            let implicit_lateral_function = matches!(right.as_ref(), FromClause::Function { .. });
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

            let mut rows = match kind {
                JoinKind::Inner | JoinKind::Cross => {
                    if matches!(kind, JoinKind::Inner) {
                        if let Some(rows) = try_hash_inner_join(
                            eval_hook,
                            &left_rows,
                            &right_rows,
                            on_expr,
                            params,
                        )? {
                            rows
                        } else {
                            cross_filter(eval_hook, &left_rows, &right_rows, on_expr, params)?
                        }
                    } else {
                        cross_filter(eval_hook, &left_rows, &right_rows, on_expr, params)?
                    }
                }
                JoinKind::Left => {
                    if let Some(rows) = try_hash_left_join(
                        engine,
                        eval_hook,
                        &left_rows,
                        &right_rows,
                        right,
                        on_expr,
                        params,
                    )? {
                        rows
                    } else {
                        left_outer(
                            &left_rows,
                            &right_rows,
                            right,
                            on_expr,
                            params,
                            engine,
                            eval_hook,
                        )?
                    }
                }
                JoinKind::Right => left_outer(
                    &right_rows,
                    &left_rows,
                    left,
                    on_expr,
                    params,
                    engine,
                    eval_hook,
                )?,
                JoinKind::Full => full_outer(
                    &left_rows,
                    &right_rows,
                    left,
                    right,
                    on_expr,
                    params,
                    engine,
                    eval_hook,
                )?,
            };
            if let Some(filter) = row_filter.as_deref_mut() {
                filter(&mut rows)?;
            }
            Ok(rows)
        }
        FromClause::Values {
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
            )?)
        }
        FromClause::Function {
            name,
            args,
            alias,
            column_aliases,
            column_types,
        } => {
            let hook = ScopedEngineHook::new(engine, ctes);
            let context = TableFunctionEvalContext::new(engine, params, &hook);
            Ok(build_table_function_rows(
                &context,
                name,
                args,
                alias.as_deref(),
                column_aliases,
                column_types,
            )?)
        }
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            let planned = ctes.planned_subqueries.get(&format!("{body:?}")).cloned();
            let result = if let Some(plan) = planned {
                let local_cte_names = plan.ctes.iter().map(|cte| cte.name.clone()).collect();
                execute_view_plan_with_parent_cache(engine, &plan, params, ctes, &local_cte_names)?
            } else {
                let local_cte_names = select_cte_names(body);
                execute_view_with_parent_cache(engine, body, params, ctes, &local_cte_names)?
            };
            Ok(materialize_subquery_rows(
                result,
                alias.as_deref(),
                column_aliases,
            ))
        }
    }
}

fn execute_view_with_parent_cache(
    engine: &Engine,
    body: &SelectStmt,
    params: &[SQLParam],
    ctes: &mut CteScope,
    local_cte_names: &BTreeSet<String>,
) -> Result<SQLResult, SQLError> {
    let saved = save_and_remove_cte_names(ctes, local_cte_names);
    let result = execute_select_ast_with_ctes(engine, body, params, ctes);
    restore_cte_names(ctes, saved);
    result
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
) -> Vec<(String, Option<Vec<ResultRow>>, Option<SelectStmt>)> {
    names
        .iter()
        .map(|name| {
            (
                name.clone(),
                ctes.remove_materialized(name),
                ctes.inlined.remove(name),
            )
        })
        .collect()
}

fn restore_cte_names(
    ctes: &mut CteScope,
    saved: Vec<(String, Option<Vec<ResultRow>>, Option<SelectStmt>)>,
) {
    for (name, rows, inlined) in saved {
        match rows {
            Some(rows) => {
                ctes.rows.insert(name.clone(), rows);
            }
            None => {
                ctes.remove_materialized(&name);
            }
        }
        match inlined {
            Some(query) => {
                ctes.inlined.insert(name, query);
            }
            None => {
                ctes.inlined.remove(&name);
            }
        }
    }
}

fn select_cte_names(stmt: &SelectStmt) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_select_cte_names(stmt, &mut names);
    names
}

fn collect_select_cte_names(stmt: &SelectStmt, names: &mut BTreeSet<String>) {
    for cte in &stmt.with {
        names.insert(cte.name.clone());
        collect_select_cte_names(&cte.query, names);
    }
    if let Some(set_op) = &stmt.set_op {
        if let Some(left) = set_op.left.as_deref() {
            collect_select_cte_names(left, names);
        }
        collect_select_cte_names(&set_op.right, names);
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
    rows: &[Vec<Expr>],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::{eval, EvalContext};
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
    let ctx = EvalContext::new(None, params).with_engine(eval_hook);
    let mut out: Vec<ResultRow> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut r = ResultRow::new();
        for (i, expr) in row.iter().enumerate() {
            let v = eval(expr, &ctx)?;
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
    right: &FromClause,
    kind: JoinKind,
    on: Option<&Expr>,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::{eval, truthy, EvalContext};
    let mut out: Vec<ResultRow> = Vec::new();
    for left_row in left_rows {
        let right_rows = match right {
            FromClause::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                let planned = ctes.planned_subqueries.get(&format!("{body:?}"));
                let physical = planned
                    .and_then(uqa_planner::QueryPlan::physical_select)
                    .unwrap_or_else(|| (**body).clone());
                let result = execute_lateral_subquery(engine, &physical, left_row, params, ctes)?;
                materialize_subquery_rows(result, alias.as_deref(), column_aliases)
            }
            FromClause::Function {
                name,
                args,
                alias,
                column_aliases,
                column_types,
            } => {
                let hook = ScopedEngineHook::new(engine, ctes);
                let context = TableFunctionEvalContext::new(engine, params, &hook);
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
                    let ctx = EvalContext::new(Some(&joined), params).with_engine(&scoped_hook);
                    truthy(&eval(filter, &ctx)?)
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
    stmt: &SelectStmt,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    let mut scoped_ctes = ctes.clone();
    materialize_ctes(engine, stmt, params, &mut scoped_ctes)?;

    let Some(from) = stmt.from.as_ref() else {
        // A FROM-less body still applies its WHERE clause against the
        // outer scope (`EXISTS (SELECT 1 WHERE false)` has no rows).
        if let Some(filter) = stmt.r#where.as_ref() {
            let ctx = EvalContext::new(Some(outer_row), params).with_engine(engine);
            if !uqa_sql::expr::truthy(&eval(filter, &ctx)?) {
                return Ok(SQLResult::from_rows(
                    projection_columns(&stmt.projections),
                    Vec::new(),
                ));
            }
        }
        let projected =
            project_join_row_with_engine(Some(engine), outer_row, &stmt.projections, params)?;
        return Ok(SQLResult::from_rows(
            projection_columns(&stmt.projections),
            vec![projected],
        ));
    };

    let inner_rows = build_join_rows_with_ctes(engine, from, params, &mut scoped_ctes)?;
    let mut filtered: Vec<ResultRow> = Vec::with_capacity(inner_rows.len());
    for inner in inner_rows {
        let merged = merge_lateral_scope_rows(outer_row, &inner);
        let keep = match stmt.r#where.as_ref() {
            None => true,
            Some(filter) => {
                let ctx = EvalContext::new(Some(&merged), params).with_engine(engine);
                uqa_sql::expr::truthy(&eval(filter, &ctx)?)
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
        let rows = aggregate_join_rows(engine, stmt, &filtered, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let ordered = apply_row_order_limit(filtered, stmt, engine, params)?;
    let columns = projection_columns(&stmt.projections);
    let rows = ordered
        .iter()
        .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
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
}

impl<'a> TableFunctionEvalContext<'a> {
    pub(super) fn new(
        engine: &'a Engine,
        params: &'a [SQLParam],
        eval_hook: &'a dyn uqa_sql::expr::EngineHook,
    ) -> Self {
        Self {
            engine,
            params,
            eval_hook,
        }
    }
}

#[allow(clippy::similar_names)]
pub(super) fn build_table_function_rows(
    context: &TableFunctionEvalContext<'_>,
    name: &str,
    args: &[Expr],
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
    args: &[Expr],
    alias: Option<&str>,
    column_aliases: &[String],
    column_types: &[String],
    row: Option<&ResultRow>,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::{evaluate_call_args, unknown_function_error, EvalContext};
    let engine = context.engine;
    let ctx = EvalContext::new(row, context.params).with_engine(context.eval_hook);
    let lower = name.to_ascii_lowercase();
    let call_args = evaluate_call_args(args, &ctx)?;
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
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    on: Option<&Expr>,
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
        let Some((left_key, right_key)) =
            decide_join_sides(eval_hook, left_rows, right_rows, lhs, rhs, params)
        else {
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
            |row| left_accessor.key(row, eval_hook, params),
            |row| right_accessor.key(row, eval_hook, params),
        )?
    } else {
        try_hash_inner_join(
            left_rows,
            right_rows,
            |row| composite_join_key(&left_keys, row, eval_hook, params),
            |row| composite_join_key(&right_keys, row, eval_hook, params),
        )?
    };
    Ok(Some(out))
}

fn try_hash_left_join(
    engine: &Engine,
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    right_from: &FromClause,
    on: Option<&Expr>,
    params: &[SQLParam],
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
        let Some((left_key, right_key)) =
            decide_join_sides(eval_hook, left_rows, right_rows, lhs, rhs, params)
        else {
            return Ok(None);
        };
        left_keys.push(left_key);
        right_keys.push(right_key);
    }

    let mut index: std::collections::HashMap<JoinKey, Vec<&ResultRow>> =
        std::collections::HashMap::with_capacity(right_rows.len());
    for row in right_rows {
        if let Some(key) = join_key_for_exprs(&right_keys, row, eval_hook, params)? {
            index.entry(key).or_default().push(row);
        }
    }

    let mut out = Vec::with_capacity(left_rows.len());
    for left in left_rows {
        let key = join_key_for_exprs(&left_keys, left, eval_hook, params)?;
        let matches = key.as_ref().and_then(|key| index.get(key));
        match matches {
            Some(rows) if !rows.is_empty() => {
                for right in rows {
                    out.push(merge_rows(left, right));
                }
            }
            _ => {
                let mut padded = left.clone();
                pad_nulls_for_from(&mut padded, right_from, engine);
                out.push(padded);
            }
        }
    }
    Ok(Some(out))
}

fn split_join_equalities(expr: &Expr) -> Option<Vec<(&Expr, &Expr)>> {
    match expr {
        Expr::Binary {
            op: uqa_sql::ast::BinaryOp::Equal,
            lhs,
            rhs,
        } => Some(vec![(lhs, rhs)]),
        Expr::And(items) => {
            let mut equalities = Vec::with_capacity(items.len());
            for item in items {
                let Expr::Binary {
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
    Expr(&'a Expr),
}

impl<'a> JoinKeyAccessor<'a> {
    fn new(expr: &'a Expr) -> Self {
        match expr {
            Expr::Column(name) => Self::Column(name.as_str()),
            Expr::QualifiedColumn {
                qualifier,
                column,
                key,
            } => Self::QualifiedColumn(if key.is_empty() {
                qualified_key(qualifier, column)
            } else {
                key.clone()
            }),
            _ => Self::Expr(expr),
        }
    }

    fn key(
        &self,
        row: &ResultRow,
        eval_hook: &dyn uqa_sql::expr::EngineHook,
        params: &[SQLParam],
    ) -> Result<Option<JoinKey>, SQLError> {
        Ok(match self {
            Self::Column(name) => value_to_join_key(column_value(row, name)),
            Self::QualifiedColumn(key) => value_to_join_key(row.get(key)),
            Self::Expr(expr) => {
                let ctx = EvalContext::new(Some(row), params).with_engine(eval_hook);
                match eval(expr, &ctx)? {
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
    exprs: &[&Expr],
    row: &ResultRow,
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    params: &[SQLParam],
) -> Result<Option<JoinKey>, SQLError> {
    let ctx = EvalContext::new(Some(row), params).with_engine(eval_hook);
    let mut values = Vec::with_capacity(exprs.len());
    for expr in exprs {
        match eval(expr, &ctx)? {
            Value::Null => return Ok(None),
            value => values.push(value),
        }
    }
    let refs: Vec<&Value> = values.iter().collect();
    Ok(Some(JoinKey::composite(&refs)))
}

fn join_key_for_exprs(
    exprs: &[&Expr],
    row: &ResultRow,
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    params: &[SQLParam],
) -> Result<Option<JoinKey>, SQLError> {
    if exprs.len() == 1 {
        JoinKeyAccessor::new(exprs[0]).key(row, eval_hook, params)
    } else {
        composite_join_key(exprs, row, eval_hook, params)
    }
}

/// Pick which expression evaluates over the left side and which over
/// the right by sampling the first row of each side. Returns
/// `(left_key_expr, right_key_expr)` when one direction works,
/// `None` when the predicate isn't separable across sides.
fn decide_join_sides<'a>(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    lhs: &'a Expr,
    rhs: &'a Expr,
    params: &[SQLParam],
) -> Option<(&'a Expr, &'a Expr)> {
    if left_rows.is_empty() || right_rows.is_empty() {
        return None;
    }
    let l_sample = &left_rows[0];
    let r_sample = &right_rows[0];
    let lhs_on_left = eval_yields_value(eval_hook, l_sample, lhs, params);
    let rhs_on_right = eval_yields_value(eval_hook, r_sample, rhs, params);
    if lhs_on_left && rhs_on_right {
        return Some((lhs, rhs));
    }
    let rhs_on_left = eval_yields_value(eval_hook, l_sample, rhs, params);
    let lhs_on_right = eval_yields_value(eval_hook, r_sample, lhs, params);
    if rhs_on_left && lhs_on_right {
        return Some((rhs, lhs));
    }
    None
}

fn eval_yields_value(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    row: &ResultRow,
    expr: &Expr,
    params: &[SQLParam],
) -> bool {
    let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(eval_hook);
    matches!(uqa_sql::expr::eval(expr, &ctx), Ok(v) if v != uqa_core::Value::Null)
}

fn cross_filter(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    on: Option<&Expr>,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::with_capacity(left_rows.len() * right_rows.len());
    for l in left_rows {
        for r in right_rows {
            let merged = merge_rows(l, r);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx = uqa_sql::expr::EvalContext::new(Some(&merged), params)
                        .with_engine(eval_hook);
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
    params: &[SQLParam],
    engine: &Engine,
    eval_hook: &dyn uqa_sql::expr::EngineHook,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for l in outer_rows {
        let mut matched = false;
        for r in inner_rows {
            let merged = merge_rows(l, r);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx = uqa_sql::expr::EvalContext::new(Some(&merged), params)
                        .with_engine(eval_hook);
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
            pad_nulls_for_from(&mut pad, inner_from, engine);
            out.push(pad);
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn full_outer(
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    left_from: &FromClause,
    right_from: &FromClause,
    on: Option<&Expr>,
    params: &[SQLParam],
    engine: &Engine,
    eval_hook: &dyn uqa_sql::expr::EngineHook,
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
                    let ctx = uqa_sql::expr::EvalContext::new(Some(&merged), params)
                        .with_engine(eval_hook);
                    uqa_sql::expr::truthy(&uqa_sql::expr::eval(expr, &ctx)?)
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
            pad_nulls_for_from(&mut padded, right_from, engine);
            out.push(padded);
        }
    }
    for (idx, right) in right_rows.iter().enumerate() {
        if matched_right[idx] {
            continue;
        }
        let mut padded = ResultRow::new();
        pad_nulls_for_from(&mut padded, left_from, engine);
        for (k, v) in right {
            padded.insert(k.clone(), v.clone());
        }
        out.push(padded);
    }
    Ok(out)
}

fn pad_nulls_for_from(row: &mut ResultRow, from: &FromClause, engine: &Engine) {
    let mut tables = Vec::new();
    from.collect_tables(&mut tables);
    for (name, alias) in &tables {
        let null_keys = null_row_for(name, alias.as_deref(), engine);
        for (k, v) in null_keys {
            row.entry(k).or_insert(v);
        }
    }
}

#[allow(dead_code)]
fn project_join_row(
    engine: &Engine,
    src: &ResultRow,
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    project_join_row_with_engine(Some(engine), src, projections, params)
}

pub(super) fn project_join_row_with_engine(
    engine: Option<&Engine>,
    src: &ResultRow,
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let hook = engine.map(|engine| engine as &dyn uqa_sql::expr::EngineHook);
    let labels = projection_columns(projections);
    project_join_row_inner(engine, hook, src, projections, &labels, params)
}

pub(super) fn project_join_row_with_hook(
    engine: Option<&dyn uqa_sql::expr::EngineHook>,
    src: &ResultRow,
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let labels = projection_columns(projections);
    project_join_row_inner(None, engine, src, projections, &labels, params)
}

pub(super) fn project_join_row_with_engine_hook(
    engine: &Engine,
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    src: &ResultRow,
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let labels = projection_columns(projections);
    project_join_row_inner(
        Some(engine),
        Some(eval_hook),
        src,
        projections,
        &labels,
        params,
    )
}

pub(super) fn project_join_row_with_hook_and_labels(
    engine: Option<&dyn uqa_sql::expr::EngineHook>,
    src: &ResultRow,
    projections: &[Projection],
    labels: &[String],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    project_join_row_inner(None, engine, src, projections, labels, params)
}

fn project_join_row_inner(
    engine: Option<&Engine>,
    eval_engine: Option<&dyn uqa_sql::expr::EngineHook>,
    src: &ResultRow,
    projections: &[Projection],
    labels: &[String],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let mut ctx = uqa_sql::expr::EvalContext::new(Some(src), params);
    if let Some(e) = eval_engine {
        ctx = ctx.with_engine(e);
    }
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
        if let Some(value) = projected_value_from_row(&proj.expr, src) {
            out.insert(label, value);
            continue;
        }
        // `uqa_highlight()` evaluates against the analyzer for the
        // matched field, which the evaluator does not have access
        // to. Intercept the call here, resolve the string column +
        // query, and emit the wrapped text through
        // `uqa_analysis::highlight`.
        if let Expr::Func { name, args, .. } = &proj.expr {
            if let Some(engine) = engine {
                if let Some(value) = engine_func_intercept(Some(engine), name, args, src, params)? {
                    out.insert(label, value);
                    continue;
                }
            }
        }
        let value = uqa_sql::expr::eval(&proj.expr, &ctx)?;
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
    args: &[Expr],
    row: &ResultRow,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "uqa_highlight" => Ok(Some(run_uqa_highlight(engine, row, args, params)?)),
        "score_bm25" | "score_bayesian_bm25" => {
            validate_score_projection_args(&lower, args, row, params)?;
            Ok(Some(
                row.get(SCORE_COLUMN).cloned().unwrap_or(Value::Float(0.0)),
            ))
        }
        "deep_learn" => Ok(Some(run_deep_learn_projection(engine, args, row, params)?)),
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
                let _ = run_graph_create(eng, args, params)?;
            }
            Ok(Some(Value::Bool(true)))
        }
        "graph_drop" => {
            if let Some(eng) = engine {
                let _ = run_graph_drop(eng, args, params)?;
            }
            Ok(Some(Value::Bool(true)))
        }
        // Apache AGE-compatible functions: strict name validation and
        // a void (SQL NULL) return value.
        "create_graph" => match engine {
            Some(eng) => Ok(Some(run_age_create_graph(eng, args, params)?)),
            None => Ok(Some(Value::Null)),
        },
        "drop_graph" => match engine {
            Some(eng) => Ok(Some(run_age_drop_graph(eng, args, params)?)),
            None => Ok(Some(Value::Null)),
        },
        _ => Ok(None),
    }
}

fn run_deep_learn_projection(
    engine: Option<&Engine>,
    args: &[Expr],
    row: &ResultRow,
    params: &[SQLParam],
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
    let ctx = EvalContext::new(Some(row), params).with_engine(engine);
    let model_name = match eval(&args[0], &ctx)? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.model must be a string, got {other:?}"
            )));
        }
    };
    let training_source = match eval(&args[1], &ctx)? {
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
    args: &[Expr],
    row: &ResultRow,
    params: &[SQLParam],
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
    let ctx = EvalContext::new(Some(row), params);
    match eval(&args[query_idx], &ctx)? {
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
    engine: Option<&Engine>,
    row: &ResultRow,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    if args.len() < 2 || args.len() > 6 {
        return Err(SQLError::BadArity {
            name: "uqa_highlight".into(),
            expected: "2..=6".into(),
            actual: args.len(),
        });
    }
    let mut ctx = uqa_sql::expr::EvalContext::new(Some(row), params);
    if let Some(e) = engine {
        ctx = ctx.with_engine(e);
    }
    let text = match &args[0] {
        Expr::Column(c) => match row.get(c) {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Null) => return Ok(Value::Null),
            Some(other) => format!("{other:?}"),
            None => return Ok(Value::Null),
        },
        Expr::QualifiedColumn {
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
            match row.get(lookup_key) {
                Some(Value::Str(s)) => s.clone(),
                Some(Value::Null) => return Ok(Value::Null),
                Some(other) => format!("{other:?}"),
                None => return Ok(Value::Null),
            }
        }
        other => match uqa_sql::expr::eval(other, &ctx)? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            v => format!("{v:?}"),
        },
    };
    let query_str = match uqa_sql::expr::eval(&args[1], &ctx)? {
        Value::Str(s) => s,
        Value::Null => return Ok(Value::Str(text)),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "uqa_highlight query must be string, got {other:?}"
            )));
        }
    };
    let start_tag = match args.get(2) {
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
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
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
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
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
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
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
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
