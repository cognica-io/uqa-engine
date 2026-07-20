//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL aggregate execution and spill buffering for blocking inputs.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::sync::Arc;

use uqa_core::{DecimalValue, Value};
use uqa_sql::ast::{BinaryOp, Expr, OrderBy, Projection, SelectStmt};
use uqa_sql::expr::{EvalContext, RowLookup};
use uqa_sql::{ResultRow, SQLError, SQLParam};
use uqa_storage::document_store::Document;

use crate::{Engine, SQLAggregateFunction, SQLAggregateState, ScoredEntry};

use super::{core_value_to_json, projection_columns};

const AGGREGATE_SPILL_ROWS: usize = 4096;

pub(super) fn aggregate_join_rows(
    engine: &Engine,
    stmt: &SelectStmt,
    rows: &[ResultRow],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    // GROUPING SETS / ROLLUP / CUBE: run the aggregator once per
    // grouping set, then concatenate the result rows. Columns that
    // aren't in the active grouping set come out as NULL.
    if !stmt.grouping_sets.is_empty() {
        let mut combined: Vec<ResultRow> = Vec::new();
        let sets = stmt.grouping_sets.clone();
        let labels = projection_columns(&stmt.projections);
        for set in sets {
            let mut sub = stmt.clone();
            sub.group_by.clone_from(&set);
            sub.grouping_sets = Vec::new();
            let part = aggregate_join_rows_relaxed(engine, &sub, rows, params)?;
            // Columns from the parent projection that aren't in the
            // active grouping set get filled with NULL on every row.
            for mut row in part {
                for (idx, proj) in stmt.projections.iter().enumerate() {
                    let label = labels[idx].clone();
                    if contains_aggregate(engine, &proj.expr) {
                        continue;
                    }
                    let in_set = set.iter().any(|g| exprs_match(&proj.expr, g));
                    if !in_set {
                        row.insert(label, Value::Null);
                    }
                }
                combined.push(row);
            }
        }
        return Ok(combined);
    }
    let agg_targets = aggregate_exprs(engine, &stmt.projections);

    let mut groups: BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)> = BTreeMap::new();

    for row in rows {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
        let group_values: Vec<Value> = stmt
            .group_by
            .iter()
            .map(|g| uqa_sql::expr::eval(g, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        let bucket = group_bucket(engine, &mut groups, group_values, &agg_targets)?;
        for (i, expr) in agg_targets.iter().enumerate() {
            let Expr::Func {
                name,
                args,
                distinct,
                order_by,
                filter,
            } = expr
            else {
                continue;
            };
            if let Some(filter_expr) = filter.as_deref() {
                let keep =
                    uqa_sql::expr::eval(filter_expr, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v));
                if !keep {
                    continue;
                }
            }
            observe_aggregate(&mut bucket.0[i], name, args, *distinct, order_by, &ctx)?;
        }
    }

    if groups.is_empty() && stmt.group_by.is_empty() {
        groups.insert(
            Vec::new(),
            (
                new_aggregate_accumulators(engine, &agg_targets)?,
                Vec::new(),
            ),
        );
    }

    let mut out = Vec::with_capacity(groups.len());
    let labels = projection_columns(&stmt.projections);
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let group_row = group_context_row(stmt, &group_values);
        let mut agg_idx = 0;
        for (idx, proj) in stmt.projections.iter().enumerate() {
            let label = labels[idx].clone();
            if contains_aggregate(engine, &proj.expr) {
                let resolved =
                    replace_aggregates_with_values(engine, &proj.expr, &accs, &mut agg_idx)?;
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&group_row), params).with_engine(engine);
                row.insert(label, uqa_sql::expr::eval(&resolved, &ctx)?);
            } else {
                if !expr_references_columns(&proj.expr) {
                    let ctx = uqa_sql::expr::EvalContext::new(Some(&group_row), params)
                        .with_engine(engine);
                    row.insert(label, uqa_sql::expr::eval(&proj.expr, &ctx)?);
                    continue;
                }
                let mut placed = false;
                for (g_expr, g_value) in stmt.group_by.iter().zip(&group_values) {
                    if exprs_match(&proj.expr, g_expr) {
                        row.insert(label.clone(), g_value.clone());
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    return Err(SQLError::Unsupported(format!(
                        "non-aggregated projection `{label}` must appear in GROUP BY"
                    )));
                }
            }
        }
        // HAVING filter: evaluated against a synthetic row that
        // contains the group-by column values plus every projection
        // alias. Aggregate references inside the HAVING expression
        // resolve through `eval_aggregate_in_having` which walks the
        // group rows to recompute the aggregate without re-projecting.
        if let Some(having_expr) = stmt.having.as_ref() {
            let resolved = resolve_having(
                engine,
                having_expr,
                &row,
                stmt,
                &accs,
                &group_values,
                params,
            )?;
            // Evaluate HAVING against the group-by column values merged with the projection
            // aliases, so it can reference grouped columns that are not themselves projected.
            let mut having_row = group_row.clone();
            for (key, value) in &row {
                having_row.insert(key.clone(), value.clone());
            }
            let ctx =
                uqa_sql::expr::EvalContext::new(Some(&having_row), params).with_engine(engine);
            let kept =
                uqa_sql::expr::eval(&resolved, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v));
            if !kept {
                continue;
            }
        }
        out.push(row);
    }
    Ok(out)
}

/// Walk a HAVING expression and replace each aggregate-function
/// reference with its computed value from the group's accumulators.
/// Non-aggregate sub-expressions (column refs, comparisons, AND / OR)
/// pass through untouched so the caller can `eval` the result.
fn resolve_having(
    engine: &Engine,
    expr: &Expr,
    _projected_row: &ResultRow,
    stmt: &SelectStmt,
    accs: &[AggregateAccumulator],
    _group_values: &[Value],
    _params: &[SQLParam],
) -> Result<Expr, SQLError> {
    fn walk(
        engine: &Engine,
        e: &Expr,
        stmt: &SelectStmt,
        accs: &[AggregateAccumulator],
    ) -> Result<Expr, SQLError> {
        if is_aggregate(engine, e) {
            // Find the matching projection so we can pluck the
            // already-computed accumulator value. Falls back to
            // matching by aggregate-function shape (name + args).
            for (idx, agg_expr) in aggregate_exprs(engine, &stmt.projections)
                .into_iter()
                .enumerate()
            {
                if exprs_match(agg_expr, e) {
                    if let Expr::Func { name, args, .. } = agg_expr {
                        let v = aggregate_value_with_args(name, &accs[idx], args)?;
                        return Ok(Expr::Literal(v));
                    }
                }
            }
            // Aggregate appears in HAVING but not in SELECT; reject.
            return Err(SQLError::Unsupported(
                "HAVING references an aggregate that is not in the SELECT list".into(),
            ));
        }
        match e {
            Expr::And(parts) => Ok(Expr::And(
                parts
                    .iter()
                    .map(|p| walk(engine, p, stmt, accs))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Expr::Or(parts) => Ok(Expr::Or(
                parts
                    .iter()
                    .map(|p| walk(engine, p, stmt, accs))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Expr::Not(inner) => Ok(Expr::Not(Box::new(walk(engine, inner, stmt, accs)?))),
            Expr::Binary { op, lhs, rhs } => Ok(Expr::Binary {
                op: *op,
                lhs: Box::new(walk(engine, lhs, stmt, accs)?),
                rhs: Box::new(walk(engine, rhs, stmt, accs)?),
            }),
            Expr::IsNull { expr, negated } => Ok(Expr::IsNull {
                expr: Box::new(walk(engine, expr, stmt, accs)?),
                negated: *negated,
            }),
            Expr::Between { expr, low, high } => Ok(Expr::Between {
                expr: Box::new(walk(engine, expr, stmt, accs)?),
                low: Box::new(walk(engine, low, stmt, accs)?),
                high: Box::new(walk(engine, high, stmt, accs)?),
            }),
            Expr::InList {
                expr,
                list,
                negated,
            } => Ok(Expr::InList {
                expr: Box::new(walk(engine, expr, stmt, accs)?),
                list: list
                    .iter()
                    .map(|x| walk(engine, x, stmt, accs))
                    .collect::<Result<Vec<_>, _>>()?,
                negated: *negated,
            }),
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
                    .map(|a| walk(engine, a, stmt, accs))
                    .collect::<Result<Vec<_>, _>>()?,
                distinct: *distinct,
                order_by: order_by.clone(),
                filter: filter.clone(),
            }),
            other => Ok(other.clone()),
        }
    }
    walk(engine, expr, stmt, accs)
}

/// Variant of [`aggregate_join_rows`] used by the GROUPING SETS
/// dispatcher: projections that aren't in the active `group_by` are
/// emitted as NULL (matching `PostgreSQL`'s ROLLUP / CUBE semantics)
/// instead of raising an error.
fn aggregate_join_rows_relaxed(
    engine: &Engine,
    stmt: &SelectStmt,
    rows: &[ResultRow],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let agg_targets = aggregate_exprs(engine, &stmt.projections);
    let mut groups: BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)> = BTreeMap::new();
    for row in rows {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
        let group_values: Vec<Value> = stmt
            .group_by
            .iter()
            .map(|g| uqa_sql::expr::eval(g, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        let bucket = group_bucket(engine, &mut groups, group_values, &agg_targets)?;
        for (i, expr) in agg_targets.iter().enumerate() {
            let Expr::Func {
                name,
                args,
                distinct,
                order_by,
                filter,
            } = expr
            else {
                continue;
            };
            if let Some(filter_expr) = filter.as_deref() {
                let keep =
                    uqa_sql::expr::eval(filter_expr, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v));
                if !keep {
                    continue;
                }
            }
            observe_aggregate(&mut bucket.0[i], name, args, *distinct, order_by, &ctx)?;
        }
    }
    if groups.is_empty() && stmt.group_by.is_empty() {
        groups.insert(
            Vec::new(),
            (
                new_aggregate_accumulators(engine, &agg_targets)?,
                Vec::new(),
            ),
        );
    }
    let mut out = Vec::with_capacity(groups.len());
    let labels = projection_columns(&stmt.projections);
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let group_row = group_context_row(stmt, &group_values);
        let mut agg_idx = 0;
        for (idx, proj) in stmt.projections.iter().enumerate() {
            let label = labels[idx].clone();
            if contains_aggregate(engine, &proj.expr) {
                let resolved =
                    replace_aggregates_with_values(engine, &proj.expr, &accs, &mut agg_idx)?;
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&group_row), params).with_engine(engine);
                row.insert(label, uqa_sql::expr::eval(&resolved, &ctx)?);
            } else {
                if !expr_references_columns(&proj.expr) {
                    let ctx = uqa_sql::expr::EvalContext::new(Some(&group_row), params)
                        .with_engine(engine);
                    row.insert(label, uqa_sql::expr::eval(&proj.expr, &ctx)?);
                    continue;
                }
                let mut placed = false;
                for (g_expr, g_value) in stmt.group_by.iter().zip(&group_values) {
                    if exprs_match(&proj.expr, g_expr) {
                        row.insert(label.clone(), g_value.clone());
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    row.insert(label, Value::Null);
                }
            }
        }
        out.push(row);
    }
    Ok(out)
}

fn exprs_match(lhs: &Expr, rhs: &Expr) -> bool {
    match (lhs, rhs) {
        (Expr::Star, Expr::Star) => true,
        (Expr::Column(a), Expr::Column(b)) => a == b,
        (
            Expr::QualifiedColumn {
                qualifier: aq,
                column: ac,
                ..
            },
            Expr::QualifiedColumn {
                qualifier: bq,
                column: bc,
                ..
            },
        ) => aq == bq && ac == bc,
        (Expr::Column(c), Expr::QualifiedColumn { column, .. })
        | (Expr::QualifiedColumn { column, .. }, Expr::Column(c)) => c == column,
        (Expr::Literal(a), Expr::Literal(b)) => literals_equal(a, b),
        (Expr::Param(a), Expr::Param(b)) => a == b,
        (
            Expr::Func {
                name: an,
                args: aa,
                distinct: ad,
                order_by: ao,
                filter: af,
            },
            Expr::Func {
                name: bn,
                args: ba,
                distinct: bd,
                order_by: bo,
                filter: bf,
            },
        ) => {
            an.eq_ignore_ascii_case(bn)
                && ad == bd
                && aa.len() == ba.len()
                && aa.iter().zip(ba.iter()).all(|(x, y)| exprs_match(x, y))
                && ao.len() == bo.len()
                && ao.iter().zip(bo.iter()).all(|(x, y)| {
                    x.descending == y.descending
                        && x.nulls == y.nulls
                        && exprs_match(&x.expr, &y.expr)
                })
                && match (af.as_deref(), bf.as_deref()) {
                    (None, None) => true,
                    (Some(x), Some(y)) => exprs_match(x, y),
                    _ => false,
                }
        }
        (
            Expr::Binary {
                op: ao,
                lhs: al,
                rhs: ar,
            },
            Expr::Binary {
                op: bo,
                lhs: bl,
                rhs: br,
            },
        ) => ao == bo && exprs_match(al, bl) && exprs_match(ar, br),
        (Expr::And(a), Expr::And(b)) | (Expr::Or(a), Expr::Or(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| exprs_match(x, y))
        }
        (Expr::Not(a), Expr::Not(b)) => exprs_match(a, b),
        (Expr::Cast { expr: a, ty: at }, Expr::Cast { expr: b, ty: bt }) => {
            at == bt && exprs_match(a, b)
        }
        _ => false,
    }
}

fn literals_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bytes(x), Value::Bytes(y)) => x == y,
        (Value::Temporal(x), Value::Temporal(y)) => x == y,
        _ => false,
    }
}

pub(super) fn has_aggregate(engine: &Engine, projections: &[Projection]) -> bool {
    projections
        .iter()
        .any(|p| contains_aggregate(engine, &p.expr))
}

fn is_aggregate(engine: &Engine, expr: &Expr) -> bool {
    matches!(expr, Expr::Func { name, .. } if matches!(
        name.to_ascii_lowercase().as_str(),
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "string_agg"
            | "array_agg"
            | "bool_and"
            | "bool_or"
            | "stddev"
            | "stddev_samp"
            | "stddev_pop"
            | "variance"
            | "var_samp"
            | "var_pop"
            | "percentile_cont"
            | "percentile_disc"
            | "mode"
            | "json_agg"
            | "jsonb_agg"
            | "json_object_agg"
            | "jsonb_object_agg"
    ) || engine.has_registered_aggregate_function(name))
}

fn aggregate_exprs<'a>(engine: &Engine, projections: &'a [Projection]) -> Vec<&'a Expr> {
    let mut out = Vec::new();
    for projection in projections {
        collect_aggregate_exprs(engine, &projection.expr, &mut out);
    }
    out
}

fn collect_aggregate_exprs<'a>(engine: &Engine, expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if is_aggregate(engine, expr) {
        out.push(expr);
        return;
    }
    match expr {
        Expr::Func { args, filter, .. } => {
            for arg in args {
                collect_aggregate_exprs(engine, arg, out);
            }
            if let Some(filter) = filter.as_deref() {
                collect_aggregate_exprs(engine, filter, out);
            }
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_aggregate_exprs(engine, item, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_aggregate_exprs(engine, lhs, out);
            collect_aggregate_exprs(engine, rhs, out);
        }
        Expr::Not(inner) | Expr::Cast { expr: inner, .. } => {
            collect_aggregate_exprs(engine, inner, out);
        }
        Expr::IsNull { expr, .. } => collect_aggregate_exprs(engine, expr, out),
        Expr::Between { expr, low, high } => {
            collect_aggregate_exprs(engine, expr, out);
            collect_aggregate_exprs(engine, low, out);
            collect_aggregate_exprs(engine, high, out);
        }
        Expr::InList { expr, list, .. } => {
            collect_aggregate_exprs(engine, expr, out);
            for item in list {
                collect_aggregate_exprs(engine, item, out);
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_deref() {
                collect_aggregate_exprs(engine, base, out);
            }
            for (condition, result) in when {
                collect_aggregate_exprs(engine, condition, out);
                collect_aggregate_exprs(engine, result, out);
            }
            if let Some(else_branch) = else_branch.as_deref() {
                collect_aggregate_exprs(engine, else_branch, out);
            }
        }
        Expr::InSubquery { expr, .. } => collect_aggregate_exprs(engine, expr, out),
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_)
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => {}
    }
}

fn contains_aggregate(engine: &Engine, expr: &Expr) -> bool {
    let mut found = Vec::new();
    collect_aggregate_exprs(engine, expr, &mut found);
    !found.is_empty()
}

/// Collect the top-level column names an expression reads. Returns
/// `false` when the expression can reach arbitrary fields (`*`,
/// subqueries, window calls), in which case callers must materialise
/// whole documents.
pub(super) fn collect_expr_columns(expr: &Expr, out: &mut BTreeSet<String>) -> bool {
    match expr {
        Expr::Column(name) => {
            out.insert(name.clone());
            true
        }
        Expr::QualifiedColumn { column, .. } => {
            out.insert(column.clone());
            true
        }
        Expr::Literal(_) | Expr::Param(_) => true,
        Expr::Func {
            args,
            filter,
            order_by,
            ..
        } => {
            args.iter().all(|a| collect_expr_columns(a, out))
                && filter
                    .as_deref()
                    .is_none_or(|f| collect_expr_columns(f, out))
                && order_by.iter().all(|o| collect_expr_columns(&o.expr, out))
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_columns(lhs, out) && collect_expr_columns(rhs, out)
        }
        Expr::Not(inner) | Expr::Cast { expr: inner, .. } => collect_expr_columns(inner, out),
        Expr::IsNull { expr, .. } => collect_expr_columns(expr, out),
        Expr::Between { expr, low, high } => {
            collect_expr_columns(expr, out)
                && collect_expr_columns(low, out)
                && collect_expr_columns(high, out)
        }
        Expr::InList { expr, list, .. } => {
            collect_expr_columns(expr, out) && list.iter().all(|i| collect_expr_columns(i, out))
        }
        Expr::And(items) | Expr::Or(items) | Expr::Array(items) => {
            items.iter().all(|i| collect_expr_columns(i, out))
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_none_or(|b| collect_expr_columns(b, out))
                && when.iter().all(|(condition, result)| {
                    collect_expr_columns(condition, out) && collect_expr_columns(result, out)
                })
                && else_branch
                    .as_deref()
                    .is_none_or(|e| collect_expr_columns(e, out))
        }
        _ => false,
    }
}

/// The column set the aggregation pass reads, or `None` when whole
/// documents are required (`*` expansion, subqueries, ...). `count(*)`
/// contributes no columns.
fn aggregation_column_projection(
    engine: &Engine,
    stmt: &SelectStmt,
    agg_targets: &[&Expr],
) -> Option<BTreeSet<String>> {
    fn safe_while_document_store_is_borrowed(expr: &Expr) -> bool {
        match expr {
            Expr::Literal(_) | Expr::Param(_) | Expr::Column(_) | Expr::QualifiedColumn { .. } => {
                true
            }
            Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
                items.iter().all(safe_while_document_store_is_borrowed)
            }
            Expr::Binary { lhs, rhs, .. } => {
                safe_while_document_store_is_borrowed(lhs)
                    && safe_while_document_store_is_borrowed(rhs)
            }
            Expr::Not(inner) | Expr::Cast { expr: inner, .. } => {
                safe_while_document_store_is_borrowed(inner)
            }
            Expr::IsNull { expr, .. } => safe_while_document_store_is_borrowed(expr),
            Expr::Between { expr, low, high } => {
                safe_while_document_store_is_borrowed(expr)
                    && safe_while_document_store_is_borrowed(low)
                    && safe_while_document_store_is_borrowed(high)
            }
            Expr::InList { expr, list, .. } => {
                safe_while_document_store_is_borrowed(expr)
                    && list.iter().all(safe_while_document_store_is_borrowed)
            }
            Expr::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref()
                    .is_none_or(safe_while_document_store_is_borrowed)
                    && when.iter().all(|(condition, result)| {
                        safe_while_document_store_is_borrowed(condition)
                            && safe_while_document_store_is_borrowed(result)
                    })
                    && else_branch
                        .as_deref()
                        .is_none_or(safe_while_document_store_is_borrowed)
            }
            // Functions can re-enter the engine through EngineHook. Keep
            // their evaluation outside the document-store read guard, along
            // with every expression form that already requires whole rows.
            Expr::Star
            | Expr::Func { .. }
            | Expr::WindowCall { .. }
            | Expr::ScalarSubquery(_)
            | Expr::Exists { .. }
            | Expr::InSubquery { .. } => false,
        }
    }

    let mut columns = BTreeSet::new();
    for group in &stmt.group_by {
        if !safe_while_document_store_is_borrowed(group)
            || !collect_expr_columns(group, &mut columns)
        {
            return None;
        }
    }
    for expr in agg_targets {
        let Expr::Func {
            name,
            args,
            filter,
            order_by,
            ..
        } = expr
        else {
            if !safe_while_document_store_is_borrowed(expr)
                || !collect_expr_columns(expr, &mut columns)
            {
                return None;
            }
            continue;
        };
        // Registered aggregate states are user code and can re-enter the
        // engine from `observe`. Materialise their rows so that callback runs
        // after the document-store read guard has been released.
        if engine.registered_aggregate_function(name).is_some() {
            return None;
        }
        let count_star =
            name.eq_ignore_ascii_case("count") && matches!(args.as_slice(), [Expr::Star]);
        if !count_star {
            for arg in args {
                if !safe_while_document_store_is_borrowed(arg)
                    || !collect_expr_columns(arg, &mut columns)
                {
                    return None;
                }
            }
        }
        if let Some(filter_expr) = filter.as_deref() {
            if !safe_while_document_store_is_borrowed(filter_expr)
                || !collect_expr_columns(filter_expr, &mut columns)
            {
                return None;
            }
        }
        for order in order_by {
            if !safe_while_document_store_is_borrowed(&order.expr)
                || !collect_expr_columns(&order.expr, &mut columns)
            {
                return None;
            }
        }
    }
    Some(columns)
}

struct ProjectedRow<'a> {
    columns: &'a ProjectedColumns,
    values: &'a [&'a Value],
}

const PROJECTED_SLOT_EMPTY: u32 = u32::MAX;
const PROJECTED_SLOT_COLLISION: u32 = u32::MAX - 1;

struct ProjectedColumns {
    // `names` is the sorted, complete set collected from every expression that
    // can be evaluated through this projection. That invariant lets the
    // single-first-byte case return its slot without repeating a string
    // comparison in the per-row hot loop.
    names: Vec<String>,
    first_byte_slots: [u32; 256],
}

impl ProjectedColumns {
    fn new(names: Vec<String>) -> Self {
        debug_assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        let mut first_byte_slots = [PROJECTED_SLOT_EMPTY; 256];
        for (index, name) in names.iter().enumerate() {
            let Some(first) = name.as_bytes().first().copied() else {
                continue;
            };
            let slot = &mut first_byte_slots[usize::from(first)];
            if *slot == PROJECTED_SLOT_EMPTY {
                *slot = u32::try_from(index).unwrap_or(PROJECTED_SLOT_COLLISION);
            } else {
                *slot = PROJECTED_SLOT_COLLISION;
            }
        }
        Self {
            names,
            first_byte_slots,
        }
    }

    fn index(&self, name: &str) -> Option<usize> {
        if let Some(first) = name.as_bytes().first().copied() {
            let slot = self.first_byte_slots[usize::from(first)];
            if slot < PROJECTED_SLOT_COLLISION {
                let index = slot as usize;
                debug_assert_eq!(self.names[index], name);
                return Some(index);
            }
        }
        self.names
            .binary_search_by(|candidate| candidate.as_str().cmp(name))
            .ok()
    }
}

fn projected_group_slots(stmt: &SelectStmt, columns: &ProjectedColumns) -> Option<Vec<usize>> {
    stmt.group_by
        .iter()
        .map(|expr| match expr {
            Expr::Column(name) => columns.index(name),
            Expr::QualifiedColumn { column, .. } => columns.index(column),
            _ => None,
        })
        .collect()
}

enum ProjectedAggregateInput {
    Evaluate,
    Slot(usize),
    CountOne,
    Expression(ProjectedExpression),
}

enum ProjectedAggregatePlan {
    /// No FILTER, DISTINCT, ORDER BY, or NULL-preserving behavior remains to
    /// dispatch per row; feed the compiled input straight into its accumulator.
    Direct(ProjectedAggregateInput),
    General(ProjectedAggregateInput),
}

enum ProjectedExpression {
    Slot(usize),
    Literal(Value),
    Binary {
        op: BinaryOp,
        lhs: Box<ProjectedExpression>,
        rhs: Box<ProjectedExpression>,
    },
}

enum ProjectedExpressionValue<'a> {
    Borrowed(&'a Value),
    Owned(Value),
}

impl ProjectedExpressionValue<'_> {
    fn as_value(&self) -> &Value {
        match self {
            Self::Borrowed(value) => value,
            Self::Owned(value) => value,
        }
    }
}

impl ProjectedExpression {
    fn compile(expr: &Expr, columns: &ProjectedColumns) -> Option<Self> {
        match expr {
            Expr::Column(name) => Some(Self::Slot(columns.index(name)?)),
            Expr::QualifiedColumn { column, .. } => Some(Self::Slot(columns.index(column)?)),
            Expr::Literal(value) => Some(Self::Literal(value.clone())),
            Expr::Binary { op, lhs, rhs } => Some(Self::Binary {
                op: *op,
                lhs: Box::new(Self::compile(lhs, columns)?),
                rhs: Box::new(Self::compile(rhs, columns)?),
            }),
            _ => None,
        }
    }

    fn evaluate<'a>(
        &'a self,
        values: &'a [&'a Value],
    ) -> Result<ProjectedExpressionValue<'a>, SQLError> {
        match self {
            Self::Slot(slot) => Ok(ProjectedExpressionValue::Borrowed(values[*slot])),
            Self::Literal(value) => Ok(ProjectedExpressionValue::Borrowed(value)),
            Self::Binary { op, lhs, rhs } => {
                let left = lhs.evaluate(values)?;
                let right = rhs.evaluate(values)?;
                Ok(ProjectedExpressionValue::Owned(
                    uqa_sql::expr::eval_binary_values(*op, left.as_value(), right.as_value())?,
                ))
            }
        }
    }
}

fn projected_aggregate_plans(
    engine: &Engine,
    agg_targets: &[&Expr],
    columns: &ProjectedColumns,
) -> Vec<ProjectedAggregatePlan> {
    agg_targets
        .iter()
        .map(|expr| {
            let Expr::Func {
                name,
                args,
                distinct,
                order_by,
                filter,
            } = expr
            else {
                return ProjectedAggregatePlan::General(ProjectedAggregateInput::Evaluate);
            };
            if engine.registered_aggregate_function(name).is_some() {
                return ProjectedAggregatePlan::General(ProjectedAggregateInput::Evaluate);
            }
            let input = if name.eq_ignore_ascii_case("count")
                && (args.is_empty() || matches!(args.as_slice(), [Expr::Star]))
            {
                ProjectedAggregateInput::CountOne
            } else if is_ordered_set_aggregate(name) || is_json_object_aggregate(name) {
                ProjectedAggregateInput::Evaluate
            } else {
                match args.first() {
                    Some(Expr::Column(name)) => columns.index(name).map_or(
                        ProjectedAggregateInput::Evaluate,
                        ProjectedAggregateInput::Slot,
                    ),
                    Some(Expr::QualifiedColumn { column, .. }) => columns.index(column).map_or(
                        ProjectedAggregateInput::Evaluate,
                        ProjectedAggregateInput::Slot,
                    ),
                    Some(expr) => ProjectedExpression::compile(expr, columns).map_or(
                        ProjectedAggregateInput::Evaluate,
                        ProjectedAggregateInput::Expression,
                    ),
                    None => ProjectedAggregateInput::Evaluate,
                }
            };
            let direct = filter.is_none()
                && !*distinct
                && order_by.is_empty()
                && !is_json_array_aggregate(name)
                && !matches!(input, ProjectedAggregateInput::Evaluate);
            if direct {
                ProjectedAggregatePlan::Direct(input)
            } else {
                ProjectedAggregatePlan::General(input)
            }
        })
        .collect()
}

impl RowLookup for ProjectedRow<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        self.columns
            .index(name)
            .and_then(|index| self.values.get(index).copied())
    }

    fn qualified_column(&self, _qualifier: &str, column: &str, _key: &str) -> Option<&Value> {
        // This view is only used by the single-table aggregation path,
        // whose stored documents carry bare field names.
        self.column(column)
    }
}

/// Feed the selected rows into an aggregate without forcing projected
/// fields through two whole-result maps. Known column projections are
/// visited in doc-id order and folded immediately; only expressions
/// that cannot declare their columns retain the whole-document fallback.
fn accumulate_table_rows(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    stmt: &SelectStmt,
    agg_targets: &[&Expr],
    groups: &mut BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)>,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    if !aggregation_needs_documents(stmt, agg_targets) {
        let empty_document = Document::new();
        for _ in scored {
            accumulate_document(engine, stmt, agg_targets, groups, &empty_document, params)?;
        }
        return Ok(());
    }
    let doc_ids: Vec<uqa_core::DocId> = scored.iter().map(|entry| entry.doc_id).collect();
    match aggregation_column_projection(engine, stmt, agg_targets) {
        Some(columns) if !columns.is_empty() => {
            let columns = ProjectedColumns::new(columns.into_iter().collect());
            let refs: Vec<&str> = columns.names.iter().map(String::as_str).collect();
            let group_slots = projected_group_slots(stmt, &columns);
            let aggregate_plans = projected_aggregate_plans(engine, agg_targets, &columns);
            let mut group_key_cache = ProjectedGroupCache::Active(Vec::new());
            let mut error = None;
            engine.for_each_document_fields_multi_ref(
                table,
                &doc_ids,
                &refs,
                &mut |_doc_id, values| {
                    let row = ProjectedRow {
                        columns: &columns,
                        values,
                    };
                    let ctx = EvalContext::from_row_lookup(&row, params).with_engine(engine);
                    let result = match group_slots.as_deref() {
                        Some(slots) => accumulate_projected_context(
                            engine,
                            agg_targets,
                            groups,
                            &mut group_key_cache,
                            &aggregate_plans,
                            values,
                            slots,
                            &ctx,
                        ),
                        None => accumulate_context(engine, stmt, agg_targets, groups, &ctx),
                    };
                    match result {
                        Ok(()) => true,
                        Err(err) => {
                            error = Some(err);
                            false
                        }
                    }
                },
            );
            if let Some(err) = error {
                Err(err)
            } else {
                flush_projected_group_cache(&mut group_key_cache, groups);
                Ok(())
            }
        }
        Some(_) => {
            let empty_document = Document::new();
            for _ in scored {
                accumulate_document(engine, stmt, agg_targets, groups, &empty_document, params)?;
            }
            Ok(())
        }
        None => {
            let documents = engine.get_documents_bulk(table, &doc_ids);
            let empty_document = Document::new();
            for entry in scored {
                let document = documents.get(&entry.doc_id).unwrap_or(&empty_document);
                accumulate_document(engine, stmt, agg_targets, groups, document, params)?;
            }
            Ok(())
        }
    }
}

fn accumulate_document(
    engine: &Engine,
    stmt: &SelectStmt,
    agg_targets: &[&Expr],
    groups: &mut BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)>,
    document: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let ctx = uqa_sql::expr::EvalContext::new(Some(document), params).with_engine(engine);
    accumulate_context(engine, stmt, agg_targets, groups, &ctx)
}

fn accumulate_context(
    engine: &Engine,
    stmt: &SelectStmt,
    agg_targets: &[&Expr],
    groups: &mut BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)>,
    ctx: &EvalContext<'_>,
) -> Result<(), SQLError> {
    let group_values: Vec<Value> = stmt
        .group_by
        .iter()
        .map(|group| uqa_sql::expr::eval(group, ctx))
        .collect::<Result<Vec<_>, _>>()?;
    let bucket = group_bucket(engine, groups, group_values, agg_targets)?;
    observe_aggregate_targets(bucket, agg_targets, ctx)
}

const PROJECTED_GROUP_CACHE_LIMIT: usize = 32;

struct ProjectedGroupCacheEntry {
    fingerprint: u64,
    key: Vec<Value>,
    bucket: (Vec<AggregateAccumulator>, Vec<Value>),
}

enum ProjectedGroupCache {
    Active(Vec<ProjectedGroupCacheEntry>),
    Disabled,
}

fn flush_projected_group_cache(
    cache: &mut ProjectedGroupCache,
    groups: &mut BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)>,
) {
    let ProjectedGroupCache::Active(entries) =
        std::mem::replace(cache, ProjectedGroupCache::Disabled)
    else {
        return;
    };
    for entry in entries {
        debug_assert!(!groups.contains_key(&entry.key));
        groups.insert(entry.key, entry.bucket);
    }
}

fn projected_group_fingerprint(values: &[&Value], group_slots: &[usize]) -> u64 {
    let mut fingerprint = 0xcbf2_9ce4_8422_2325;
    for index in group_slots {
        fingerprint_value(&mut fingerprint, values[*index]);
    }
    fingerprint
}

fn fingerprint_value(fingerprint: &mut u64, value: &Value) {
    fn write(fingerprint: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *fingerprint ^= u64::from(*byte);
            *fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    match value {
        Value::Null => write(fingerprint, &[0]),
        // `Value::cmp` compares Bool / Int / Float / Decimal across types.
        // Give every numeric value one conservative token so values that
        // compare equal can never land in different fingerprint buckets.
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Decimal(_) => {
            write(fingerprint, &[1]);
        }
        Value::Str(value) => {
            write(fingerprint, &[2]);
            write(fingerprint, &(value.len() as u64).to_le_bytes());
            write(fingerprint, value.as_bytes());
        }
        Value::Bytes(value) => {
            write(fingerprint, &[3]);
            write(fingerprint, &(value.len() as u64).to_le_bytes());
            write(fingerprint, value);
        }
        Value::Temporal(_) => write(fingerprint, &[4]),
        Value::List(values) => {
            write(fingerprint, &[5]);
            write(fingerprint, &(values.len() as u64).to_le_bytes());
            for value in values {
                fingerprint_value(fingerprint, value);
            }
        }
        Value::Map(values) => {
            write(fingerprint, &[6]);
            write(fingerprint, &(values.len() as u64).to_le_bytes());
            for (key, value) in values {
                write(fingerprint, &(key.len() as u64).to_le_bytes());
                write(fingerprint, key.as_bytes());
                fingerprint_value(fingerprint, value);
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the hot loop passes independent borrowed state without a context wrapper"
)]
fn accumulate_projected_context(
    engine: &Engine,
    agg_targets: &[&Expr],
    groups: &mut BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)>,
    group_key_cache: &mut ProjectedGroupCache,
    aggregate_plans: &[ProjectedAggregatePlan],
    values: &[&Value],
    group_slots: &[usize],
    ctx: &EvalContext<'_>,
) -> Result<(), SQLError> {
    // A global aggregate has one empty-key bucket. The one-entry B-tree
    // lookup is cheaper than fingerprinting and probing the bounded cache.
    if group_slots.is_empty() {
        if groups.is_empty() {
            let bucket = group_bucket(engine, groups, Vec::new(), agg_targets)?;
            return observe_projected_aggregate_targets(
                bucket,
                agg_targets,
                aggregate_plans,
                values,
                ctx,
            );
        }
        let bucket = groups
            .get_mut(&[] as &[Value])
            .ok_or_else(|| SQLError::Internal("global aggregate bucket missing".into()))?;
        return observe_projected_aggregate_targets(
            bucket,
            agg_targets,
            aggregate_plans,
            values,
            ctx,
        );
    }

    if let ProjectedGroupCache::Active(entries) = group_key_cache {
        let fingerprint = projected_group_fingerprint(values, group_slots);
        if let Some(index) = entries.iter().position(|entry| {
            entry.fingerprint == fingerprint
                && entry.key.len() == group_slots.len()
                && entry
                    .key
                    .iter()
                    .zip(group_slots)
                    .all(|(stored, index)| stored.cmp(values[*index]) == Ordering::Equal)
        }) {
            return observe_projected_aggregate_targets(
                &mut entries[index].bucket,
                agg_targets,
                aggregate_plans,
                values,
                ctx,
            );
        }

        if entries.len() < PROJECTED_GROUP_CACHE_LIMIT {
            let group_values: Vec<Value> = group_slots
                .iter()
                .map(|index| values[*index].clone())
                .collect();
            let bucket = (
                new_aggregate_accumulators(engine, agg_targets)?,
                group_values.clone(),
            );
            entries.push(ProjectedGroupCacheEntry {
                fingerprint,
                key: group_values,
                bucket,
            });
            return observe_projected_aggregate_targets(
                &mut entries.last_mut().expect("cached group inserted").bucket,
                agg_targets,
                aggregate_plans,
                values,
                ctx,
            );
        }
    }

    // A 33rd distinct key makes this a high-cardinality aggregation.
    // Flush the bounded cache exactly once and retain the general B-tree
    // path for all remaining rows.
    flush_projected_group_cache(group_key_cache, groups);
    let group_values: Vec<Value> = group_slots
        .iter()
        .map(|index| values[*index].clone())
        .collect();
    let bucket = group_bucket(engine, groups, group_values, agg_targets)?;
    observe_projected_aggregate_targets(bucket, agg_targets, aggregate_plans, values, ctx)
}

fn observe_projected_aggregate_targets(
    bucket: &mut (Vec<AggregateAccumulator>, Vec<Value>),
    agg_targets: &[&Expr],
    aggregate_plans: &[ProjectedAggregatePlan],
    values: &[&Value],
    ctx: &EvalContext<'_>,
) -> Result<(), SQLError> {
    for (index, plan) in aggregate_plans.iter().enumerate() {
        let ProjectedAggregatePlan::General(input) = plan else {
            match plan {
                ProjectedAggregatePlan::Direct(ProjectedAggregateInput::Slot(slot)) => {
                    bucket.0[index].observe(values[*slot])?;
                }
                ProjectedAggregatePlan::Direct(ProjectedAggregateInput::CountOne) => {
                    debug_assert!(matches!(
                        bucket.0[index].state_plan,
                        AggregateStatePlan::Count
                    ));
                    bucket.0[index].count += 1;
                }
                ProjectedAggregatePlan::Direct(ProjectedAggregateInput::Expression(expression)) => {
                    let value = expression.evaluate(values)?;
                    bucket.0[index].observe(value.as_value())?;
                }
                ProjectedAggregatePlan::Direct(ProjectedAggregateInput::Evaluate)
                | ProjectedAggregatePlan::General(_) => unreachable!("direct aggregate plan"),
            }
            continue;
        };
        let Some(expr) = agg_targets.get(index) else {
            return Err(SQLError::Internal(
                "projected aggregate plan lost its expression".into(),
            ));
        };
        let Expr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } = expr
        else {
            continue;
        };
        if let Some(filter_expr) = filter.as_deref() {
            let keep = uqa_sql::expr::eval(filter_expr, ctx)
                .is_ok_and(|value| uqa_sql::expr::truthy(&value));
            if !keep {
                continue;
            }
        }
        match input {
            ProjectedAggregateInput::Slot(slot) => observe_builtin_aggregate_value(
                &mut bucket.0[index],
                name,
                values[*slot],
                *distinct,
                order_by,
                ctx,
            )?,
            ProjectedAggregateInput::CountOne => observe_builtin_aggregate_value(
                &mut bucket.0[index],
                name,
                &Value::Int(1),
                *distinct,
                order_by,
                ctx,
            )?,
            ProjectedAggregateInput::Evaluate => {
                observe_aggregate(&mut bucket.0[index], name, args, *distinct, order_by, ctx)?;
            }
            ProjectedAggregateInput::Expression(expression) => {
                let value = expression.evaluate(values)?;
                observe_builtin_aggregate_value(
                    &mut bucket.0[index],
                    name,
                    value.as_value(),
                    *distinct,
                    order_by,
                    ctx,
                )?;
            }
        }
    }
    Ok(())
}

fn observe_aggregate_targets(
    bucket: &mut (Vec<AggregateAccumulator>, Vec<Value>),
    agg_targets: &[&Expr],
    ctx: &EvalContext<'_>,
) -> Result<(), SQLError> {
    for (index, expr) in agg_targets.iter().enumerate() {
        let Expr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } = expr
        else {
            continue;
        };
        if let Some(filter_expr) = filter.as_deref() {
            let keep = uqa_sql::expr::eval(filter_expr, ctx)
                .is_ok_and(|value| uqa_sql::expr::truthy(&value));
            if !keep {
                continue;
            }
        }
        observe_aggregate(&mut bucket.0[index], name, args, *distinct, order_by, ctx)?;
    }
    Ok(())
}

/// Whether the aggregation pass has to materialise stored documents.
/// `count(*)` (with no column-referencing FILTER) and literal-argument
/// aggregates can run on doc ids alone, which turns `SELECT count(*)
/// FROM t` into a no-fetch pass over the match list.
fn aggregation_needs_documents(stmt: &SelectStmt, agg_targets: &[&Expr]) -> bool {
    if stmt.group_by.iter().any(expr_references_columns) {
        return true;
    }
    agg_targets.iter().any(|expr| {
        let Expr::Func {
            name,
            args,
            filter,
            order_by,
            ..
        } = expr
        else {
            return expr_references_columns(expr);
        };
        let count_star =
            name.eq_ignore_ascii_case("count") && matches!(args.as_slice(), [Expr::Star]);
        (!count_star && args.iter().any(expr_references_columns))
            || filter.as_deref().is_some_and(expr_references_columns)
            || order_by.iter().any(|o| expr_references_columns(&o.expr))
    })
}

fn expr_references_columns(expr: &Expr) -> bool {
    match expr {
        Expr::Star | Expr::Column(_) | Expr::QualifiedColumn { .. } => true,
        Expr::Func { args, filter, .. } => {
            args.iter().any(expr_references_columns)
                || filter.as_deref().is_some_and(expr_references_columns)
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().any(expr_references_columns)
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_references_columns(lhs) || expr_references_columns(rhs)
        }
        Expr::Not(inner) | Expr::Cast { expr: inner, .. } => expr_references_columns(inner),
        Expr::IsNull { expr, .. } => expr_references_columns(expr),
        Expr::Between { expr, low, high } => {
            expr_references_columns(expr)
                || expr_references_columns(low)
                || expr_references_columns(high)
        }
        Expr::InList { expr, list, .. } => {
            expr_references_columns(expr) || list.iter().any(expr_references_columns)
        }
        Expr::WindowCall { args, .. } => args.iter().any(expr_references_columns),
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(expr_references_columns)
                || when.iter().any(|(condition, result)| {
                    expr_references_columns(condition) || expr_references_columns(result)
                })
                || else_branch.as_deref().is_some_and(expr_references_columns)
        }
        Expr::InSubquery { expr, .. } => expr_references_columns(expr),
        Expr::ScalarSubquery(_) | Expr::Exists { .. } => true,
        Expr::Literal(_) | Expr::Param(_) => false,
    }
}

fn group_context_row(stmt: &SelectStmt, group_values: &[Value]) -> ResultRow {
    let mut row = ResultRow::new();
    for (expr, value) in stmt.group_by.iter().zip(group_values) {
        match expr {
            Expr::Column(column) => {
                row.insert(column.clone(), value.clone());
            }
            Expr::QualifiedColumn {
                qualifier,
                column,
                key,
            } => {
                if key.is_empty() {
                    row.insert(format!("{qualifier}.{column}"), value.clone());
                } else {
                    row.insert(key.clone(), value.clone());
                }
                row.insert(column.clone(), value.clone());
            }
            _ => {}
        }
    }
    row
}

fn replace_aggregates_with_values(
    engine: &Engine,
    expr: &Expr,
    accs: &[AggregateAccumulator],
    cursor: &mut usize,
) -> Result<Expr, SQLError> {
    if is_aggregate(engine, expr) {
        let Expr::Func { name, args, .. } = expr else {
            return Err(SQLError::Internal("aggregate expr lost".into()));
        };
        let Some(acc) = accs.get(*cursor) else {
            return Err(SQLError::Internal("aggregate accumulator missing".into()));
        };
        *cursor += 1;
        return Ok(Expr::Literal(aggregate_value_with_args(name, acc, args)?));
    }
    match expr {
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
                .map(|arg| replace_aggregates_with_values(engine, arg, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_deref()
                .map(|filter| {
                    replace_aggregates_with_values(engine, filter, accs, cursor).map(Box::new)
                })
                .transpose()?,
        }),
        Expr::Array(items) => Ok(Expr::Array(
            items
                .iter()
                .map(|item| replace_aggregates_with_values(engine, item, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Expr::Binary { op, lhs, rhs } => Ok(Expr::Binary {
            op: *op,
            lhs: Box::new(replace_aggregates_with_values(engine, lhs, accs, cursor)?),
            rhs: Box::new(replace_aggregates_with_values(engine, rhs, accs, cursor)?),
        }),
        Expr::Not(inner) => Ok(Expr::Not(Box::new(replace_aggregates_with_values(
            engine, inner, accs, cursor,
        )?))),
        Expr::And(parts) => Ok(Expr::And(
            parts
                .iter()
                .map(|part| replace_aggregates_with_values(engine, part, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Expr::Or(parts) => Ok(Expr::Or(
            parts
                .iter()
                .map(|part| replace_aggregates_with_values(engine, part, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Expr::IsNull { expr, negated } => Ok(Expr::IsNull {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            negated: *negated,
        }),
        Expr::Between { expr, low, high } => Ok(Expr::Between {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            low: Box::new(replace_aggregates_with_values(engine, low, accs, cursor)?),
            high: Box::new(replace_aggregates_with_values(engine, high, accs, cursor)?),
        }),
        Expr::InList {
            expr,
            list,
            negated,
        } => Ok(Expr::InList {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            list: list
                .iter()
                .map(|item| replace_aggregates_with_values(engine, item, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
            negated: *negated,
        }),
        Expr::Case {
            base,
            when,
            else_branch,
        } => Ok(Expr::Case {
            base: base
                .as_deref()
                .map(|base| {
                    replace_aggregates_with_values(engine, base, accs, cursor).map(Box::new)
                })
                .transpose()?,
            when: when
                .iter()
                .map(|(condition, result)| {
                    Ok((
                        replace_aggregates_with_values(engine, condition, accs, cursor)?,
                        replace_aggregates_with_values(engine, result, accs, cursor)?,
                    ))
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            else_branch: else_branch
                .as_deref()
                .map(|branch| {
                    replace_aggregates_with_values(engine, branch, accs, cursor).map(Box::new)
                })
                .transpose()?,
        }),
        Expr::Cast { expr, ty } => Ok(Expr::Cast {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            ty: ty.clone(),
        }),
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => Ok(Expr::InSubquery {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            body: body.clone(),
            negated: *negated,
        }),
        other => Ok(other.clone()),
    }
}

fn aggregate_input_value(
    name: &str,
    args: &[Expr],
    order_by: &[OrderBy],
    ctx: &EvalContext<'_>,
) -> Result<Value, SQLError> {
    if name.eq_ignore_ascii_case("count") && (args.is_empty() || matches!(args, [Expr::Star])) {
        return Ok(Value::Int(1));
    }
    // Ordered-set aggregates: the percentile / mode fraction is a
    // direct positional argument; the value to fold comes from
    // `WITHIN GROUP (ORDER BY ...)` which the compiler parks in
    // `order_by[0]`.
    if is_ordered_set_aggregate(name) {
        return order_by
            .first()
            .map(|ob| uqa_sql::expr::eval(&ob.expr, ctx))
            .transpose()
            .map(|v| v.unwrap_or(Value::Null));
    }
    if is_json_object_aggregate(name) {
        return match args {
            [key_expr, value_expr] => {
                let key = uqa_sql::expr::eval(key_expr, ctx)?;
                if matches!(key, Value::Null) {
                    return Ok(Value::Null);
                }
                let value = uqa_sql::expr::eval(value_expr, ctx)?;
                Ok(Value::List(vec![key, value]))
            }
            _ => Err(SQLError::TypeMismatch(format!(
                "{name} requires 2 arguments"
            ))),
        };
    }
    if is_json_array_aggregate(name) {
        return match args {
            [arg] => uqa_sql::expr::eval(arg, ctx),
            _ => Err(SQLError::TypeMismatch(format!(
                "{name} requires 1 argument"
            ))),
        };
    }
    let arg = args
        .first()
        .ok_or_else(|| SQLError::Internal("aggregate missing arg".into()))?;
    uqa_sql::expr::eval(arg, ctx)
}

fn aggregate_input_values(args: &[Expr], ctx: &EvalContext<'_>) -> Result<Vec<Value>, SQLError> {
    args.iter()
        .map(|arg| match arg {
            Expr::Star => Ok(Value::Int(1)),
            other => uqa_sql::expr::eval(other, ctx),
        })
        .collect()
}

fn new_aggregate_accumulators(
    engine: &Engine,
    agg_targets: &[&Expr],
) -> Result<Vec<AggregateAccumulator>, SQLError> {
    agg_targets
        .iter()
        .map(|expr| match expr {
            Expr::Func { name, .. } => Ok(engine.registered_aggregate_function(name).map_or_else(
                || AggregateAccumulator::builtin(name),
                AggregateAccumulator::registered,
            )),
            _ => Ok(AggregateAccumulator::default()),
        })
        .collect()
}

fn group_bucket<'a>(
    engine: &Engine,
    groups: &'a mut BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)>,
    group_values: Vec<Value>,
    agg_targets: &[&Expr],
) -> Result<&'a mut (Vec<AggregateAccumulator>, Vec<Value>), SQLError> {
    use std::collections::btree_map::Entry;

    match groups.entry(group_values.clone()) {
        Entry::Occupied(entry) => Ok(entry.into_mut()),
        Entry::Vacant(entry) => {
            let accs = new_aggregate_accumulators(engine, agg_targets)?;
            Ok(entry.insert((accs, group_values)))
        }
    }
}

fn observe_aggregate(
    acc: &mut AggregateAccumulator,
    name: &str,
    args: &[Expr],
    distinct: bool,
    order_by: &[OrderBy],
    ctx: &EvalContext<'_>,
) -> Result<(), SQLError> {
    if acc.registered.is_some() {
        let values = aggregate_input_values(args, ctx)?;
        if distinct {
            let key = distinct_key(&Value::List(values.clone()));
            if !acc.distinct.insert(key) {
                return Ok(());
            }
        }
        let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
        for ob in order_by {
            let v = uqa_sql::expr::eval(&ob.expr, ctx)?;
            sort_keys.push((v, ob.descending));
        }
        acc.observe_registered(values, sort_keys)?;
        return Ok(());
    }

    let value = aggregate_input_value(name, args, order_by, ctx)?;
    observe_builtin_aggregate_value(acc, name, &value, distinct, order_by, ctx)
}

fn observe_builtin_aggregate_value(
    acc: &mut AggregateAccumulator,
    name: &str,
    value: &Value,
    distinct: bool,
    order_by: &[OrderBy],
    ctx: &EvalContext<'_>,
) -> Result<(), SQLError> {
    let preserves_null_inputs = is_json_array_aggregate(name);
    if distinct && (preserves_null_inputs || !matches!(value, Value::Null)) {
        let key = distinct_key(value);
        if !acc.distinct.insert(key) {
            return Ok(());
        }
    }
    let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
    for ob in order_by {
        let v = uqa_sql::expr::eval(&ob.expr, ctx)?;
        sort_keys.push((v, ob.descending));
    }
    if preserves_null_inputs {
        acc.observe_including_null(value, sort_keys)?;
    } else if order_by.is_empty() {
        acc.observe(value)?;
    } else {
        acc.observe_with_sort_keys(value, sort_keys)?;
    }
    Ok(())
}

fn is_json_array_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("json_agg") || name.eq_ignore_ascii_case("jsonb_agg")
}

fn is_json_object_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("json_object_agg") || name.eq_ignore_ascii_case("jsonb_object_agg")
}

fn is_ordered_set_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("percentile_cont")
        || name.eq_ignore_ascii_case("percentile_disc")
        || name.eq_ignore_ascii_case("mode")
}

pub(super) struct AggregateAccumulator {
    registered: Option<Arc<dyn SQLAggregateFunction>>,
    registered_state: Option<Box<dyn SQLAggregateState>>,
    registered_ordered: RegisteredAggregateBuffer,
    count: u64,
    sum: f64,
    integer_sum: i128,
    decimal_sum: Option<DecimalValue>,
    has_decimal: bool,
    has_float: bool,
    min: Option<Value>,
    max: Option<Value>,
    /// Distinct-bookkeeping. Filled by the dispatcher when the
    /// aggregate was annotated with `DISTINCT`. Holds canonical-form
    /// keys so `Int(1)` and `Float(1.0)` collapse to the same bucket.
    distinct: std::collections::BTreeSet<String>,
    /// Only collection, ordered-set, and statistical aggregates need
    /// their complete input. Streaming aggregates keep constant-size
    /// state and must not spill values that their finalizer never reads.
    state_plan: AggregateStatePlan,
    values: AggregateValueBuffer,
    all_values_int: bool,
    /// Boolean folds for `BOOL_AND` / `BOOL_OR`. Stay `None` until the
    /// first observation so an empty input set returns `NULL` (matches
    /// `PostgreSQL`).
    bool_and: Option<bool>,
    bool_or: Option<bool>,
}

#[derive(Clone, Copy)]
enum AggregateStatePlan {
    /// Conservative fallback for an aggregate whose state requirements
    /// are not known here.
    Generic,
    Count,
    Sum,
    Min,
    Max,
    BoolAnd,
    BoolOr,
    Buffered,
    BufferedWithCount,
}

impl AggregateStatePlan {
    fn builtin(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "count" => Self::Count,
            "sum" | "avg" => Self::Sum,
            "min" => Self::Min,
            "max" => Self::Max,
            "bool_and" => Self::BoolAnd,
            "bool_or" => Self::BoolOr,
            "stddev" | "stddev_samp" | "stddev_pop" | "variance" | "var_samp" | "var_pop" => {
                Self::BufferedWithCount
            }
            "string_agg" | "array_agg" | "json_agg" | "jsonb_agg" | "json_object_agg"
            | "jsonb_object_agg" | "percentile_cont" | "percentile_disc" | "mode" => Self::Buffered,
            _ => Self::Generic,
        }
    }

    fn retains_values(self) -> bool {
        matches!(
            self,
            Self::Generic | Self::Buffered | Self::BufferedWithCount
        )
    }
}

impl Default for AggregateAccumulator {
    fn default() -> Self {
        Self {
            registered: None,
            registered_state: None,
            registered_ordered: RegisteredAggregateBuffer::default(),
            count: 0,
            sum: 0.0,
            integer_sum: 0,
            decimal_sum: None,
            has_decimal: false,
            has_float: false,
            min: None,
            max: None,
            distinct: BTreeSet::new(),
            state_plan: AggregateStatePlan::Generic,
            values: AggregateValueBuffer::default(),
            all_values_int: true,
            bool_and: None,
            bool_or: None,
        }
    }
}

impl AggregateAccumulator {
    pub(super) fn builtin(name: &str) -> Self {
        Self {
            state_plan: AggregateStatePlan::builtin(name),
            ..Self::default()
        }
    }

    fn registered(function: Arc<dyn SQLAggregateFunction>) -> Self {
        let state = function.create_state();
        Self {
            registered: Some(function),
            registered_state: Some(state),
            ..Self::default()
        }
    }

    pub(super) fn observe(&mut self, value: &Value) -> Result<(), SQLError> {
        if matches!(value, Value::Null) {
            return Ok(());
        }
        self.observe_state(value)?;
        if self.state_plan.retains_values() {
            self.values.push(value.clone(), Vec::new())?;
        }
        Ok(())
    }

    fn observe_state(&mut self, value: &Value) -> Result<(), SQLError> {
        match self.state_plan {
            AggregateStatePlan::Generic => {
                self.count += 1;
                self.observe_sum(value)?;
                self.observe_min(value);
                self.observe_max(value);
                self.observe_bool_and(value);
                self.observe_bool_or(value);
            }
            AggregateStatePlan::Count => self.count += 1,
            AggregateStatePlan::Sum => {
                self.count += 1;
                self.observe_sum(value)?;
            }
            AggregateStatePlan::Min => self.observe_min(value),
            AggregateStatePlan::Max => self.observe_max(value),
            AggregateStatePlan::BoolAnd => self.observe_bool_and(value),
            AggregateStatePlan::BoolOr => self.observe_bool_or(value),
            AggregateStatePlan::Buffered => {}
            AggregateStatePlan::BufferedWithCount => self.count += 1,
        }
        Ok(())
    }

    fn observe_sum(&mut self, value: &Value) -> Result<(), SQLError> {
        if !matches!(value, Value::Int(_)) && self.all_values_int {
            self.all_values_int = false;
            // Integer-only SUM/AVG finalizers use `integer_sum` directly.
            // Seed the floating accumulator once, at the first non-integer,
            // instead of converting and adding every integer row twice.
            self.sum = self.integer_sum as f64;
        }
        match value {
            Value::Int(n) => {
                self.integer_sum = self
                    .integer_sum
                    .checked_add(i128::from(*n))
                    .ok_or_else(|| SQLError::TypeMismatch("integer aggregate overflow".into()))?;
                if self.has_decimal {
                    let next = DecimalValue::from_i64(*n);
                    self.decimal_sum = Some(
                        self.decimal_sum
                            .as_ref()
                            .and_then(|sum| sum.checked_add(&next))
                            .ok_or_else(|| {
                                SQLError::TypeMismatch("decimal aggregate overflow".into())
                            })?,
                    );
                }
            }
            Value::Decimal(d) => {
                let next = match &self.decimal_sum {
                    Some(sum) => sum.checked_add(d),
                    None if self.integer_sum == 0 => Some(d.clone()),
                    None => DecimalValue::parse(&self.integer_sum.to_string())
                        .and_then(|sum| sum.checked_add(d)),
                }
                .ok_or_else(|| SQLError::TypeMismatch("decimal aggregate overflow".into()))?;
                self.decimal_sum = Some(next);
                self.has_decimal = true;
            }
            Value::Float(_) => {
                self.has_float = true;
            }
            _ => {}
        }
        if !self.all_values_int {
            if let Ok(f) = value_as_f64(value) {
                self.sum += f;
            }
        }
        Ok(())
    }

    fn observe_min(&mut self, value: &Value) {
        match &self.min {
            Some(cur) if !value_lt(value, cur) => {}
            _ => self.min = Some(value.clone()),
        }
    }

    fn observe_max(&mut self, value: &Value) {
        match &self.max {
            Some(cur) if !value_gt(value, cur) => {}
            _ => self.max = Some(value.clone()),
        }
    }

    fn observe_bool_and(&mut self, value: &Value) {
        if let Value::Bool(b) = value {
            self.bool_and = Some(self.bool_and.unwrap_or(true) && *b);
        }
    }

    fn observe_bool_or(&mut self, value: &Value) {
        if let Value::Bool(b) = value {
            self.bool_or = Some(self.bool_or.unwrap_or(false) || *b);
        }
    }

    fn observe_with_sort_keys(
        &mut self,
        value: &Value,
        keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        if matches!(value, Value::Null) {
            return Ok(());
        }
        self.observe_state(value)?;
        if self.state_plan.retains_values() {
            self.values.push(value.clone(), keys)?;
        }
        Ok(())
    }

    fn observe_including_null(
        &mut self,
        value: &Value,
        keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        self.values.push(value.clone(), keys)
    }

    fn observe_registered(
        &mut self,
        values: Vec<Value>,
        sort_keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        if sort_keys.is_empty() {
            let state = self
                .registered_state
                .as_mut()
                .ok_or_else(|| SQLError::Internal("registered aggregate state missing".into()))?;
            state.observe(&values)?;
            return Ok(());
        }
        self.registered_ordered.push(values, sort_keys)
    }

    fn registered_value(&self) -> Option<Result<Value, SQLError>> {
        let function = self.registered.as_ref()?;
        if self.registered_ordered.is_empty() {
            let state = self
                .registered_state
                .as_ref()
                .ok_or_else(|| SQLError::Internal("registered aggregate state missing".into()));
            return Some(state.and_then(|state| state.finish()));
        }
        Some((|| {
            let mut state = function.create_state();
            self.registered_ordered
                .observe_ordered_into(state.as_mut())?;
            state.finish()
        })())
    }
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct AggregateValueRecord {
    value: Value,
    sort_keys: Vec<(Value, bool)>,
    sequence: u64,
}

#[derive(Default)]
struct AggregateValueBuffer {
    rows: Vec<AggregateValueRecord>,
    runs: Vec<tempfile::NamedTempFile>,
    next_sequence: u64,
    has_sort_keys: bool,
}

impl AggregateValueBuffer {
    fn push(&mut self, value: Value, sort_keys: Vec<(Value, bool)>) -> Result<(), SQLError> {
        self.has_sort_keys |= !sort_keys.is_empty();
        self.rows.push(AggregateValueRecord {
            value,
            sort_keys,
            sequence: self.next_sequence,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
        if self.rows.len() >= AGGREGATE_SPILL_ROWS {
            self.flush_run()?;
        }
        Ok(())
    }

    fn values(&self) -> Result<Vec<Value>, SQLError> {
        let mut records = self.records()?;
        records.sort_by_key(|record| record.sequence);
        Ok(records.into_iter().map(|record| record.value).collect())
    }

    fn ordered_values(&self) -> Result<Vec<Value>, SQLError> {
        let mut records = self.records()?;
        if self.has_sort_keys {
            records.sort_by(compare_aggregate_value_records);
        } else {
            records.sort_by_key(|record| record.sequence);
        }
        Ok(records.into_iter().map(|record| record.value).collect())
    }

    fn records(&self) -> Result<Vec<AggregateValueRecord>, SQLError> {
        let mut records = Vec::with_capacity(self.rows.len());
        for run in &self.runs {
            records.extend(read_aggregate_value_run(run)?);
        }
        records.extend(self.rows.iter().cloned());
        Ok(records)
    }

    fn flush_run(&mut self) -> Result<(), SQLError> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let mut run = tempfile::NamedTempFile::new().map_err(|err| {
            SQLError::Internal(format!("failed to create aggregate spill file: {err}"))
        })?;
        {
            let mut writer = BufWriter::new(run.as_file_mut());
            for row in self.rows.drain(..) {
                serde_json::to_writer(&mut writer, &row).map_err(|err| {
                    SQLError::Internal(format!("failed to serialize aggregate spill row: {err}"))
                })?;
                writer.write_all(b"\n").map_err(|err| {
                    SQLError::Internal(format!("failed to write aggregate spill row: {err}"))
                })?;
            }
            writer.flush().map_err(|err| {
                SQLError::Internal(format!("failed to flush aggregate spill file: {err}"))
            })?;
        }
        run.as_file_mut().seek(SeekFrom::Start(0)).map_err(|err| {
            SQLError::Internal(format!("failed to rewind aggregate spill file: {err}"))
        })?;
        self.runs.push(run);
        Ok(())
    }
}

fn read_aggregate_value_run(
    run: &tempfile::NamedTempFile,
) -> Result<Vec<AggregateValueRecord>, SQLError> {
    let file = run.reopen().map_err(|err| {
        SQLError::Internal(format!("failed to reopen aggregate spill file: {err}"))
    })?;
    let mut reader = BufReader::new(file);
    let mut records = Vec::new();
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).map_err(|err| {
            SQLError::Internal(format!("failed to read aggregate spill row: {err}"))
        })?;
        if bytes == 0 {
            break;
        }
        let record = serde_json::from_str(line.trim_end()).map_err(|err| {
            SQLError::Internal(format!("failed to deserialize aggregate spill row: {err}"))
        })?;
        records.push(record);
    }
    Ok(records)
}

fn compare_aggregate_value_records(a: &AggregateValueRecord, b: &AggregateValueRecord) -> Ordering {
    for ((av, ad), (bv, _bd)) in a.sort_keys.iter().zip(b.sort_keys.iter()) {
        let cmp = av.cmp(bv);
        let cmp = if *ad { cmp.reverse() } else { cmp };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    a.sequence.cmp(&b.sequence)
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct RegisteredAggregateRecord {
    values: Vec<Value>,
    sort_keys: Vec<(Value, bool)>,
    sequence: u64,
}

#[derive(Default)]
struct RegisteredAggregateBuffer {
    rows: Vec<RegisteredAggregateRecord>,
    runs: Vec<tempfile::NamedTempFile>,
    next_sequence: u64,
}

impl RegisteredAggregateBuffer {
    fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.runs.is_empty()
    }

    fn push(&mut self, values: Vec<Value>, sort_keys: Vec<(Value, bool)>) -> Result<(), SQLError> {
        self.rows.push(RegisteredAggregateRecord {
            values,
            sort_keys,
            sequence: self.next_sequence,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
        if self.rows.len() >= AGGREGATE_SPILL_ROWS {
            self.flush_run()?;
        }
        Ok(())
    }

    fn observe_ordered_into(&self, state: &mut dyn SQLAggregateState) -> Result<(), SQLError> {
        if self.runs.is_empty() {
            let mut rows = self.rows.clone();
            rows.sort_by(compare_registered_aggregate_records);
            for row in rows {
                state.observe(&row.values)?;
            }
            return Ok(());
        }

        let mut rows = self.rows.clone();
        rows.sort_by(compare_registered_aggregate_records);
        let mut readers = Vec::with_capacity(self.runs.len() + usize::from(!rows.is_empty()));
        if !rows.is_empty() {
            readers.push(RegisteredAggregateRunReader::memory(rows));
        }
        for run in &self.runs {
            readers.push(RegisteredAggregateRunReader::file(run)?);
        }

        while let Some((idx, _)) = readers
            .iter()
            .enumerate()
            .filter_map(|(idx, reader)| reader.current().map(|record| (idx, record)))
            .min_by(|(_, a), (_, b)| compare_registered_aggregate_records(a, b))
        {
            let record = readers[idx].take_current()?;
            state.observe(&record.values)?;
        }
        Ok(())
    }

    fn flush_run(&mut self) -> Result<(), SQLError> {
        if self.rows.is_empty() {
            return Ok(());
        }
        self.rows.sort_by(compare_registered_aggregate_records);
        let mut run = tempfile::NamedTempFile::new().map_err(|err| {
            SQLError::Internal(format!(
                "failed to create registered aggregate spill file: {err}"
            ))
        })?;
        {
            let mut writer = BufWriter::new(run.as_file_mut());
            for row in self.rows.drain(..) {
                serde_json::to_writer(&mut writer, &row).map_err(|err| {
                    SQLError::Internal(format!(
                        "failed to serialize registered aggregate spill row: {err}"
                    ))
                })?;
                writer.write_all(b"\n").map_err(|err| {
                    SQLError::Internal(format!(
                        "failed to write registered aggregate spill row: {err}"
                    ))
                })?;
            }
            writer.flush().map_err(|err| {
                SQLError::Internal(format!(
                    "failed to flush registered aggregate spill file: {err}"
                ))
            })?;
        }
        run.as_file_mut().seek(SeekFrom::Start(0)).map_err(|err| {
            SQLError::Internal(format!(
                "failed to rewind registered aggregate spill file: {err}"
            ))
        })?;
        self.runs.push(run);
        Ok(())
    }
}

enum RegisteredAggregateRunReader {
    Memory {
        rows: std::vec::IntoIter<RegisteredAggregateRecord>,
        current: Option<RegisteredAggregateRecord>,
    },
    File {
        reader: BufReader<File>,
        current: Option<RegisteredAggregateRecord>,
    },
}

impl RegisteredAggregateRunReader {
    fn memory(rows: Vec<RegisteredAggregateRecord>) -> Self {
        let mut rows = rows.into_iter();
        let current = rows.next();
        Self::Memory { rows, current }
    }

    fn file(run: &tempfile::NamedTempFile) -> Result<Self, SQLError> {
        let file = run.reopen().map_err(|err| {
            SQLError::Internal(format!(
                "failed to reopen registered aggregate spill file: {err}"
            ))
        })?;
        let mut reader = BufReader::new(file);
        let current = read_registered_aggregate_record(&mut reader)?;
        Ok(Self::File { reader, current })
    }

    fn current(&self) -> Option<&RegisteredAggregateRecord> {
        match self {
            Self::Memory { current, .. } | Self::File { current, .. } => current.as_ref(),
        }
    }

    fn take_current(&mut self) -> Result<RegisteredAggregateRecord, SQLError> {
        match self {
            Self::Memory { rows, current } => {
                let record = current.take().ok_or_else(|| {
                    SQLError::Internal("registered aggregate memory run exhausted".into())
                })?;
                *current = rows.next();
                Ok(record)
            }
            Self::File { reader, current } => {
                let record = current.take().ok_or_else(|| {
                    SQLError::Internal("registered aggregate spill run exhausted".into())
                })?;
                *current = read_registered_aggregate_record(reader)?;
                Ok(record)
            }
        }
    }
}

fn read_registered_aggregate_record(
    reader: &mut BufReader<File>,
) -> Result<Option<RegisteredAggregateRecord>, SQLError> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line).map_err(|err| {
        SQLError::Internal(format!(
            "failed to read registered aggregate spill row: {err}"
        ))
    })?;
    if bytes == 0 {
        return Ok(None);
    }
    serde_json::from_str(line.trim_end())
        .map(Some)
        .map_err(|err| {
            SQLError::Internal(format!(
                "failed to deserialize registered aggregate spill row: {err}"
            ))
        })
}

fn compare_registered_aggregate_records(
    a: &RegisteredAggregateRecord,
    b: &RegisteredAggregateRecord,
) -> Ordering {
    for ((av, ad), (bv, _bd)) in a.sort_keys.iter().zip(b.sort_keys.iter()) {
        let cmp = av.cmp(bv);
        let cmp = if *ad { cmp.reverse() } else { cmp };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    a.sequence.cmp(&b.sequence)
}

/// Canonical-form key for `DISTINCT` deduplication. Mirrors the
/// approach in `uqa_execution::relational::distinct_key`.
fn distinct_key(v: &Value) -> String {
    match v {
        Value::Null => "\x00".into(),
        Value::Bool(b) => format!("b:{b}"),
        Value::Int(n) => format!("i:{n}"),
        Value::Float(f) => format!("f:{f}"),
        Value::Decimal(d) => format!("n:{}", d.to_canonical_string()),
        Value::Str(s) => format!("s:{s}"),
        Value::Bytes(b) => format!("y:{}", b.len()),
        Value::Temporal(t) => format!("t:{}", t.to_sql_string()),
        other => format!("o:{other:?}"),
    }
}

fn value_as_f64(v: &Value) -> Result<f64, SQLError> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::Decimal(d) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch(format!("expected number that fits float, got {v:?}"))
        }),
        other => Err(SQLError::TypeMismatch(format!(
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
        (Value::Decimal(x), Value::Decimal(y)) => x < y,
        (Value::Int(x), Value::Decimal(y)) => DecimalValue::from_i64(*x) < *y,
        (Value::Decimal(x), Value::Int(y)) => *x < DecimalValue::from_i64(*y),
        (Value::Float(x), Value::Decimal(y)) => {
            DecimalValue::from_f64_lossy(*x).is_some_and(|x| x < *y)
        }
        (Value::Decimal(x), Value::Float(y)) => {
            DecimalValue::from_f64_lossy(*y).is_some_and(|y| *x < y)
        }
        (Value::Str(x), Value::Str(y)) => x < y,
        (Value::Temporal(x), Value::Temporal(y)) => x < y,
        _ => false,
    }
}

fn value_gt(a: &Value, b: &Value) -> bool {
    value_lt(b, a)
}

pub(super) fn build_aggregate_rows(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    // GROUPING SETS / ROLLUP / CUBE: run the aggregator per set, then
    // mask out columns not in the active set with NULL.
    if !stmt.grouping_sets.is_empty() {
        let mut combined: Vec<ResultRow> = Vec::new();
        let labels = projection_columns(&stmt.projections);
        for set in &stmt.grouping_sets {
            let mut sub = stmt.clone();
            sub.group_by.clone_from(set);
            sub.grouping_sets = Vec::new();
            let part = build_aggregate_rows_relaxed(engine, table, scored, &sub, params)?;
            for mut row in part {
                for (idx, proj) in stmt.projections.iter().enumerate() {
                    let label = labels[idx].clone();
                    if contains_aggregate(engine, &proj.expr) {
                        continue;
                    }
                    let in_set = set.iter().any(|g| exprs_match(&proj.expr, g));
                    if !in_set {
                        row.insert(label, Value::Null);
                    }
                }
                combined.push(row);
            }
        }
        return Ok(combined);
    }
    // group_key -> per-aggregate accumulator vector + the raw group key
    // values used to project the GROUP BY columns.
    let mut groups: BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)> = BTreeMap::new();
    let agg_targets = aggregate_exprs(engine, &stmt.projections);

    accumulate_table_rows(
        engine,
        table,
        scored,
        stmt,
        &agg_targets,
        &mut groups,
        params,
    )?;

    if groups.is_empty() && stmt.group_by.is_empty() {
        // SELECT count(*) FROM t with no rows still produces a row of
        // zeros so downstream consumers see a stable shape.
        groups.insert(
            Vec::new(),
            (
                new_aggregate_accumulators(engine, &agg_targets)?,
                Vec::new(),
            ),
        );
    }

    let mut rows: Vec<ResultRow> = Vec::with_capacity(groups.len());
    let labels = projection_columns(&stmt.projections);
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let group_row = group_context_row(stmt, &group_values);
        let mut agg_idx = 0;
        for (idx, proj) in stmt.projections.iter().enumerate() {
            let label = labels[idx].clone();
            if contains_aggregate(engine, &proj.expr) {
                let resolved =
                    replace_aggregates_with_values(engine, &proj.expr, &accs, &mut agg_idx)?;
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&group_row), params).with_engine(engine);
                row.insert(label, uqa_sql::expr::eval(&resolved, &ctx)?);
            } else {
                if !expr_references_columns(&proj.expr) {
                    let ctx = uqa_sql::expr::EvalContext::new(Some(&group_row), params)
                        .with_engine(engine);
                    row.insert(label, uqa_sql::expr::eval(&proj.expr, &ctx)?);
                    continue;
                }
                // Match a non-aggregate projection against the GROUP BY
                // key list using `exprs_match`, which understands both
                // bare column refs and complex expressions.
                let mut placed = false;
                for (g_expr, g_value) in stmt.group_by.iter().zip(&group_values) {
                    if exprs_match(&proj.expr, g_expr) {
                        row.insert(label.clone(), g_value.clone());
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    return Err(SQLError::Unsupported(format!(
                        "non-aggregated projection `{label}` must appear in GROUP BY"
                    )));
                }
            }
        }
        if let Some(having_expr) = stmt.having.as_ref() {
            let resolved = resolve_having(
                engine,
                having_expr,
                &row,
                stmt,
                &accs,
                &group_values,
                params,
            )?;
            // Evaluate HAVING against the group-by column values merged with the projection
            // aliases, so it can reference grouped columns that are not themselves projected.
            let mut having_row = group_row.clone();
            for (key, value) in &row {
                having_row.insert(key.clone(), value.clone());
            }
            let ctx =
                uqa_sql::expr::EvalContext::new(Some(&having_row), params).with_engine(engine);
            let kept =
                uqa_sql::expr::eval(&resolved, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v));
            if !kept {
                continue;
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Single-table aggregator variant used by the GROUPING SETS
/// dispatcher: projections that aren't in the active `group_by` come
/// out as NULL instead of erroring (`PostgreSQL` ROLLUP / CUBE
/// semantics).
fn build_aggregate_rows_relaxed(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut groups: BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)> = BTreeMap::new();
    let agg_targets = aggregate_exprs(engine, &stmt.projections);
    accumulate_table_rows(
        engine,
        table,
        scored,
        stmt,
        &agg_targets,
        &mut groups,
        params,
    )?;
    if groups.is_empty() && stmt.group_by.is_empty() {
        groups.insert(
            Vec::new(),
            (
                new_aggregate_accumulators(engine, &agg_targets)?,
                Vec::new(),
            ),
        );
    }
    let mut rows: Vec<ResultRow> = Vec::with_capacity(groups.len());
    let labels = projection_columns(&stmt.projections);
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let group_row = group_context_row(stmt, &group_values);
        let mut agg_idx = 0;
        for (idx, proj) in stmt.projections.iter().enumerate() {
            let label = labels[idx].clone();
            if contains_aggregate(engine, &proj.expr) {
                let resolved =
                    replace_aggregates_with_values(engine, &proj.expr, &accs, &mut agg_idx)?;
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&group_row), params).with_engine(engine);
                row.insert(label, uqa_sql::expr::eval(&resolved, &ctx)?);
            } else if !expr_references_columns(&proj.expr) {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&group_row), params).with_engine(engine);
                row.insert(label, uqa_sql::expr::eval(&proj.expr, &ctx)?);
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
                    row.insert(label, Value::Null);
                }
            } else {
                // Complex non-aggregate projections in ROLLUP / CUBE
                // also fall back to NULL.
                row.insert(label, Value::Null);
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

pub(super) fn aggregate_value(name: &str, acc: &AggregateAccumulator) -> Result<Value, SQLError> {
    aggregate_value_with_args(name, acc, &[])
}

fn aggregate_value_with_args(
    name: &str,
    acc: &AggregateAccumulator,
    args: &[Expr],
) -> Result<Value, SQLError> {
    if let Some(value) = acc.registered_value() {
        return value;
    }
    let lname = name.to_ascii_lowercase();

    let value = match lname.as_str() {
        "count" => Value::Int(acc.count as i64),
        "sum" => {
            if acc.count == 0 {
                Value::Null
            } else if acc.has_decimal && !acc.has_float {
                acc.decimal_sum.clone().map_or(Value::Null, Value::Decimal)
            } else if acc.all_values_int {
                Value::Int(
                    acc.integer_sum
                        .clamp(i128::from(i64::MIN), i128::from(i64::MAX))
                        as i64,
                )
            } else {
                Value::Float(acc.sum)
            }
        }
        "avg" => {
            if acc.count == 0 {
                Value::Null
            } else if acc.has_decimal && !acc.has_float {
                let divisor = DecimalValue::from_i64(acc.count as i64);
                acc.decimal_sum
                    .as_ref()
                    .and_then(|sum| sum.checked_div(&divisor))
                    .map_or(Value::Null, Value::Decimal)
            } else if acc.all_values_int {
                Value::Float(acc.integer_sum as f64 / acc.count as f64)
            } else {
                Value::Float(acc.sum / acc.count as f64)
            }
        }
        "min" => acc.min.clone().unwrap_or(Value::Null),
        "max" => acc.max.clone().unwrap_or(Value::Null),
        "string_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            // Separator: literal second positional arg, or empty.
            let sep = match args.get(1) {
                Some(Expr::Literal(Value::Str(s))) => s.clone(),
                _ => String::new(),
            };
            let parts: Vec<String> = ordered_values
                .iter()
                .filter_map(|v| match v {
                    Value::Str(s) => Some(s.clone()),
                    Value::Int(n) => Some(n.to_string()),
                    Value::Float(f) => Some(f.to_string()),
                    Value::Decimal(d) => Some(d.to_sql_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    Value::Temporal(t) => Some(t.to_sql_string()),
                    _ => None,
                })
                .collect();
            Value::Str(parts.join(&sep))
        }
        "array_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            Value::List(ordered_values)
        }
        "json_agg" | "jsonb_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            Value::List(ordered_values)
        }
        "json_object_agg" | "jsonb_object_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            let mut map = BTreeMap::new();
            for value in ordered_values {
                let Value::List(pair) = value else {
                    continue;
                };
                if pair.len() != 2 || matches!(pair[0], Value::Null) {
                    continue;
                }
                map.insert(aggregate_json_key(&pair[0]), pair[1].clone());
            }
            if map.is_empty() {
                Value::Null
            } else {
                Value::Map(map)
            }
        }
        "bool_and" => match acc.bool_and {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        },
        "bool_or" => match acc.bool_or {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        },
        "stddev" | "stddev_samp" => {
            if acc.count < 2 {
                return Ok(Value::Null);
            }
            let values = acc.values.values()?;
            statistical_aggregate_value(&values, stddev_samp(&values))
        }
        "stddev_pop" => {
            if acc.count == 0 {
                return Ok(Value::Null);
            }
            let values = acc.values.values()?;
            statistical_aggregate_value(&values, stddev_pop(&values))
        }
        "variance" | "var_samp" => {
            if acc.count < 2 {
                return Ok(Value::Null);
            }
            let values = acc.values.values()?;
            statistical_aggregate_value(&values, variance_samp(&values))
        }
        "var_pop" => {
            if acc.count == 0 {
                return Ok(Value::Null);
            }
            let values = acc.values.values()?;
            statistical_aggregate_value(&values, variance_pop(&values))
        }
        "percentile_cont" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            let frac = percentile_fraction(args);
            Value::Float(percentile_cont(&ordered_values, frac))
        }
        "percentile_disc" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            let frac = percentile_fraction(args);
            percentile_disc(&ordered_values, frac)
        }
        "mode" => mode_value(&acc.values.ordered_values()?),
        _ => Value::Null,
    };
    Ok(value)
}

fn percentile_fraction(args: &[Expr]) -> f64 {
    match args.first() {
        Some(Expr::Literal(Value::Float(f))) => *f,
        Some(Expr::Literal(Value::Int(n))) => *n as f64,
        Some(Expr::Literal(Value::Decimal(d))) => d.to_f64().unwrap_or(0.5),
        _ => 0.5,
    }
}

fn aggregate_json_key(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) => s.clone(),
        Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::List(_) | Value::Map(_) => serde_json::to_string(&core_value_to_json(value))
            .unwrap_or_else(|_| format!("{value:?}")),
    }
}

fn mean(values: &[Value]) -> f64 {
    let nums: Vec<f64> = values.iter().filter_map(|v| value_as_f64(v).ok()).collect();
    if nums.is_empty() {
        0.0
    } else {
        nums.iter().sum::<f64>() / nums.len() as f64
    }
}

/// Statistical aggregates (`variance`, `stddev_*`) return `numeric`
/// for integer / numeric inputs in `PostgreSQL` (rendering with a
/// decimal point, e.g. `1.00000...`), and `double precision` only for
/// float inputs.
fn statistical_aggregate_value(values: &[Value], computed: f64) -> Value {
    let float_input = values.iter().any(|v| matches!(v, Value::Float(_)));
    if float_input || !computed.is_finite() {
        return Value::Float(computed);
    }
    uqa_core::DecimalValue::parse(&format!("{computed:.16}"))
        .map_or(Value::Float(computed), Value::Decimal)
}

fn variance_samp(values: &[Value]) -> f64 {
    let nums: Vec<f64> = values.iter().filter_map(|v| value_as_f64(v).ok()).collect();
    if nums.len() < 2 {
        return 0.0;
    }
    let m = mean(values);
    let total: f64 = nums.iter().map(|x| (x - m).powi(2)).sum();
    total / (nums.len() as f64 - 1.0)
}

fn variance_pop(values: &[Value]) -> f64 {
    let nums: Vec<f64> = values.iter().filter_map(|v| value_as_f64(v).ok()).collect();
    if nums.is_empty() {
        return 0.0;
    }
    let m = mean(values);
    let total: f64 = nums.iter().map(|x| (x - m).powi(2)).sum();
    total / nums.len() as f64
}

fn stddev_samp(values: &[Value]) -> f64 {
    variance_samp(values).sqrt()
}

fn stddev_pop(values: &[Value]) -> f64 {
    variance_pop(values).sqrt()
}

fn percentile_cont(values: &[Value], frac: f64) -> f64 {
    let mut nums: Vec<f64> = values.iter().filter_map(|v| value_as_f64(v).ok()).collect();
    if nums.is_empty() {
        return 0.0;
    }
    nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let frac = frac.clamp(0.0, 1.0);
    let pos = frac * (nums.len() as f64 - 1.0);
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        return nums[lo];
    }
    let weight = pos - lo as f64;
    nums[lo] * (1.0 - weight) + nums[hi] * weight
}

fn percentile_disc(values: &[Value], frac: f64) -> Value {
    let mut sorted: Vec<&Value> = values.iter().collect();
    sorted.sort();
    if sorted.is_empty() {
        return Value::Null;
    }
    let frac = frac.clamp(0.0, 1.0);
    // PostgreSQL: smallest rank where cumulative cum_dist >= frac.
    let n = sorted.len();
    let mut idx = (frac * n as f64).ceil() as usize;
    if idx == 0 {
        idx = 1;
    }
    if idx > n {
        idx = n;
    }
    sorted[idx - 1].clone()
}

fn mode_value(values: &[Value]) -> Value {
    use std::collections::BTreeMap;
    if values.is_empty() {
        return Value::Null;
    }
    let mut counts: BTreeMap<String, (Value, u64)> = BTreeMap::new();
    for v in values {
        let key = distinct_key(v);
        let entry = counts.entry(key).or_insert((v.clone(), 0));
        entry.1 += 1;
    }
    counts
        .into_values()
        .max_by_key(|(_, n)| *n)
        .map_or(Value::Null, |(v, _)| v)
}

/// Compute a projection's output column name. `PostgreSQL` reports
/// standalone expressions as `?column?`; `projection_columns` adds a
/// suffix when the row map needs unique keys.
pub(super) fn projection_label_at(proj: &Projection) -> String {
    if let Some(a) = &proj.alias {
        return a.clone();
    }
    match &proj.expr {
        Expr::Column(c) => c.clone(),
        Expr::QualifiedColumn { column, .. } => column.clone(),
        Expr::Star => "*".into(),
        Expr::Func { name, .. } => name.clone(),
        _ => "?column?".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct NoopRegisteredAggregate;

    impl SQLAggregateState for NoopRegisteredAggregate {
        fn observe(&mut self, _args: &[Value]) -> Result<(), SQLError> {
            Ok(())
        }

        fn finish(&self) -> Result<Value, SQLError> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn registered_aggregates_do_not_run_under_the_document_store_guard() {
        let engine = Engine::new();
        engine
            .register_aggregate_function("registered_agg", NoopRegisteredAggregate::default)
            .unwrap();
        let mut statements = uqa_sql::compile("SELECT registered_agg(value) FROM samples").unwrap();
        let uqa_sql::ast::Statement::Select(stmt) = statements.remove(0) else {
            panic!("expected SELECT");
        };
        let agg_targets = aggregate_exprs(&engine, &stmt.projections);

        assert!(aggregation_column_projection(&engine, &stmt, &agg_targets).is_none());
    }

    #[test]
    fn streaming_aggregate_does_not_retain_or_spill_inputs() {
        let mut accumulator = AggregateAccumulator::builtin("sum");
        let end = AGGREGATE_SPILL_ROWS as i64 + 1;
        for value in 0..end {
            accumulator.observe(&Value::Int(value)).unwrap();
        }

        assert!(accumulator.values.rows.is_empty());
        assert!(accumulator.values.runs.is_empty());
        assert_eq!(
            aggregate_value("sum", &accumulator).unwrap(),
            Value::Int(end * (end - 1) / 2)
        );
    }

    #[test]
    fn collection_aggregate_still_retains_inputs() {
        let mut accumulator = AggregateAccumulator::builtin("array_agg");
        accumulator.observe(&Value::Int(7)).unwrap();

        assert_eq!(accumulator.count, 0);
        assert_eq!(accumulator.decimal_sum, None);
        assert_eq!(accumulator.min, None);
        assert_eq!(accumulator.max, None);
        assert_eq!(accumulator.values.rows.len(), 1);
        assert_eq!(
            aggregate_value("array_agg", &accumulator).unwrap(),
            Value::List(vec![Value::Int(7)])
        );
    }

    #[test]
    fn builtins_only_update_state_used_by_their_finalizer() {
        let mut count = AggregateAccumulator::builtin("count");
        count.observe(&Value::Int(7)).unwrap();
        assert_eq!(count.count, 1);
        assert_eq!(count.decimal_sum, None);
        assert_eq!(count.min, None);
        assert_eq!(count.max, None);

        let mut sum = AggregateAccumulator::builtin("sum");
        sum.observe(&Value::Int(7)).unwrap();
        assert_eq!(sum.count, 1);
        assert_eq!(sum.integer_sum, 7);
        assert_eq!(sum.decimal_sum, None);
        assert_eq!(sum.min, None);
        assert_eq!(sum.max, None);

        let mut min = AggregateAccumulator::builtin("min");
        min.observe(&Value::Int(7)).unwrap();
        assert_eq!(min.count, 0);
        assert_eq!(min.decimal_sum, None);
        assert_eq!(min.min, Some(Value::Int(7)));
        assert_eq!(min.max, None);

        let mut bool_or = AggregateAccumulator::builtin("bool_or");
        bool_or.observe(&Value::Bool(true)).unwrap();
        assert_eq!(bool_or.count, 0);
        assert_eq!(bool_or.bool_and, None);
        assert_eq!(bool_or.bool_or, Some(true));
    }

    #[test]
    fn statistical_aggregate_counts_and_retains_without_unrelated_state() {
        let mut accumulator = AggregateAccumulator::builtin("stddev_pop");
        accumulator.observe(&Value::Int(7)).unwrap();

        assert_eq!(accumulator.count, 1);
        assert_eq!(accumulator.decimal_sum, None);
        assert_eq!(accumulator.min, None);
        assert_eq!(accumulator.max, None);
        assert_eq!(accumulator.values.rows.len(), 1);
    }

    #[test]
    fn integer_sum_stays_exact_beyond_float_precision() {
        let mut accumulator = AggregateAccumulator::builtin("sum");
        accumulator
            .observe(&Value::Int(9_007_199_254_740_992))
            .unwrap();
        accumulator.observe(&Value::Int(1)).unwrap();

        assert_eq!(
            aggregate_value("sum", &accumulator).unwrap(),
            Value::Int(9_007_199_254_740_993)
        );
        assert_eq!(accumulator.decimal_sum, None);
    }

    #[test]
    fn integer_average_promotes_to_float_only_when_finalized_or_mixed() {
        let mut integers = AggregateAccumulator::builtin("avg");
        integers.observe(&Value::Int(2)).unwrap();
        integers.observe(&Value::Int(3)).unwrap();
        assert_eq!(integers.sum, 0.0);
        assert_eq!(
            aggregate_value("avg", &integers).unwrap(),
            Value::Float(2.5)
        );

        integers.observe(&Value::Float(1.5)).unwrap();
        assert_eq!(integers.sum, 6.5);
        assert_eq!(
            aggregate_value("avg", &integers).unwrap(),
            Value::Float(6.5 / 3.0)
        );
    }

    #[test]
    fn decimal_sum_absorbs_integers_observed_before_and_after_it() {
        let mut accumulator = AggregateAccumulator::builtin("sum");
        accumulator.observe(&Value::Int(2)).unwrap();
        accumulator
            .observe(&Value::Decimal(DecimalValue::parse("0.5").unwrap()))
            .unwrap();
        accumulator.observe(&Value::Int(3)).unwrap();

        assert_eq!(
            aggregate_value("sum", &accumulator).unwrap(),
            Value::Decimal(DecimalValue::parse("5.5").unwrap())
        );
    }

    #[test]
    fn projected_columns_use_direct_unique_slots_and_collision_fallback() {
        let columns =
            ProjectedColumns::new(vec!["amount".into(), "apple".into(), "quantity".into()]);

        assert_eq!(columns.index("amount"), Some(0));
        assert_eq!(columns.index("apple"), Some(1));
        assert_eq!(columns.index("quantity"), Some(2));
        assert_eq!(columns.index("missing"), None);
    }

    #[test]
    fn projected_expression_reuses_sql_binary_semantics() {
        let columns = ProjectedColumns::new(vec!["discount".into(), "price".into()]);
        let expression = Expr::Binary {
            op: BinaryOp::Divide,
            lhs: Box::new(Expr::Binary {
                op: BinaryOp::Multiply,
                lhs: Box::new(Expr::Column("price".into())),
                rhs: Box::new(Expr::Binary {
                    op: BinaryOp::Subtract,
                    lhs: Box::new(Expr::Literal(Value::Int(100))),
                    rhs: Box::new(Expr::Column("discount".into())),
                }),
            }),
            rhs: Box::new(Expr::Literal(Value::Int(100))),
        };
        let plan = ProjectedExpression::compile(&expression, &columns).unwrap();
        let discount = Value::Int(10);
        let price = Value::Int(10_000);

        assert_eq!(
            plan.evaluate(&[&discount, &price]).unwrap().as_value(),
            &Value::Int(9_000)
        );

        let null = Value::Null;
        assert_eq!(
            plan.evaluate(&[&null, &price]).unwrap().as_value(),
            &Value::Null
        );
    }

    #[test]
    fn group_fingerprint_preserves_cross_numeric_comparison_equivalence() {
        fn fingerprint(value: &Value) -> u64 {
            projected_group_fingerprint(&[value], &[0])
        }

        let int = Value::Int(1);
        let float = Value::Float(1.0);
        let decimal = Value::Decimal(DecimalValue::from_i64(1));
        let boolean = Value::Bool(true);
        assert_eq!(int.cmp(&float), Ordering::Equal);
        assert_eq!(fingerprint(&int), fingerprint(&float));
        assert_eq!(fingerprint(&int), fingerprint(&decimal));
        assert_eq!(fingerprint(&int), fingerprint(&boolean));
        assert_ne!(
            fingerprint(&Value::Str("A".into())),
            fingerprint(&Value::Str("R".into()))
        );
    }
}
