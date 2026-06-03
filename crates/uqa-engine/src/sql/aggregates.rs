//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL aggregate execution and registered aggregate spill buffering.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::sync::Arc;

use uqa_core::Value;
use uqa_sql::ast::{Expr, OrderBy, Projection, SelectStmt};
use uqa_sql::expr::EvalContext;
use uqa_sql::{ResultRow, SQLError, SQLParam};

use crate::{Engine, SQLAggregateFunction, SQLAggregateState, ScoredEntry};

use super::{core_value_to_json, projection_columns};

const REGISTERED_AGGREGATE_SPILL_ROWS: usize = 4096;

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
            let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(engine);
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
            },
            Expr::QualifiedColumn {
                qualifier: bq,
                column: bc,
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
            Expr::QualifiedColumn { qualifier, column } => {
                row.insert(format!("{qualifier}.{column}"), value.clone());
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
    match (name.to_ascii_lowercase().as_str(), args) {
        ("count", [Expr::Star]) | ("count", []) => Ok(Value::Int(1)),
        // Ordered-set aggregates: the percentile / mode fraction is a
        // direct positional argument; the value to fold comes from
        // `WITHIN GROUP (ORDER BY ...)` which the compiler parks in
        // `order_by[0]`.
        ("percentile_cont" | "percentile_disc" | "mode", _) => order_by
            .first()
            .map(|ob| uqa_sql::expr::eval(&ob.expr, ctx))
            .transpose()
            .map(|v| v.unwrap_or(Value::Null)),
        ("json_object_agg" | "jsonb_object_agg", [key_expr, value_expr]) => {
            let key = uqa_sql::expr::eval(key_expr, ctx)?;
            if matches!(key, Value::Null) {
                return Ok(Value::Null);
            }
            let value = uqa_sql::expr::eval(value_expr, ctx)?;
            Ok(Value::List(vec![key, value]))
        }
        ("json_object_agg" | "jsonb_object_agg", _) => Err(SQLError::TypeMismatch(format!(
            "{name} requires 2 arguments"
        ))),
        (_, args) => {
            let arg = args
                .first()
                .ok_or_else(|| SQLError::Internal("aggregate missing arg".into()))?;
            uqa_sql::expr::eval(arg, ctx)
        }
    }
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
                AggregateAccumulator::default,
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
    if distinct && !matches!(value, Value::Null) {
        let key = distinct_key(&value);
        if !acc.distinct.insert(key) {
            return Ok(());
        }
    }
    let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
    for ob in order_by {
        let v = uqa_sql::expr::eval(&ob.expr, ctx)?;
        sort_keys.push((v, ob.descending));
    }
    if order_by.is_empty() {
        acc.observe(&value);
    } else {
        acc.observe_with_sort_keys(&value, sort_keys);
    }
    Ok(())
}

pub(super) struct AggregateAccumulator {
    registered: Option<Arc<dyn SQLAggregateFunction>>,
    registered_state: Option<Box<dyn SQLAggregateState>>,
    registered_ordered: RegisteredAggregateBuffer,
    count: u64,
    sum: f64,
    min: Option<Value>,
    max: Option<Value>,
    /// Distinct-bookkeeping. Filled by the dispatcher when the
    /// aggregate was annotated with `DISTINCT`. Holds canonical-form
    /// keys so `Int(1)` and `Float(1.0)` collapse to the same bucket.
    distinct: std::collections::BTreeSet<String>,
    /// Every observed (non-null) value for collection-style
    /// aggregates (`STRING_AGG`, `ARRAY_AGG`, statistical aggregates,
    /// percentile / mode). Sort keys for ordered aggregates land in
    /// `sort_keys` parallel to this vector.
    values: Vec<Value>,
    /// Optional sort key per `values` entry, packed as a `Vec<(key,
    /// descending)>` so multi-key ORDER BY composes lexicographically.
    sort_keys: Vec<Vec<(Value, bool)>>,
    /// Boolean folds for `BOOL_AND` / `BOOL_OR`. Stay `None` until the
    /// first observation so an empty input set returns `NULL` (matches
    /// `PostgreSQL`).
    bool_and: Option<bool>,
    bool_or: Option<bool>,
}

impl Default for AggregateAccumulator {
    fn default() -> Self {
        Self {
            registered: None,
            registered_state: None,
            registered_ordered: RegisteredAggregateBuffer::default(),
            count: 0,
            sum: 0.0,
            min: None,
            max: None,
            distinct: BTreeSet::new(),
            values: Vec::new(),
            sort_keys: Vec::new(),
            bool_and: None,
            bool_or: None,
        }
    }
}

impl AggregateAccumulator {
    fn registered(function: Arc<dyn SQLAggregateFunction>) -> Self {
        let state = function.create_state();
        Self {
            registered: Some(function),
            registered_state: Some(state),
            ..Self::default()
        }
    }

    pub(super) fn observe(&mut self, value: &Value) {
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
        self.values.push(value.clone());
        self.sort_keys.push(Vec::new());
        if let Value::Bool(b) = value {
            self.bool_and = Some(self.bool_and.unwrap_or(true) && *b);
            self.bool_or = Some(self.bool_or.unwrap_or(false) || *b);
        }
    }

    fn observe_with_sort_keys(&mut self, value: &Value, keys: Vec<(Value, bool)>) {
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
        self.values.push(value.clone());
        self.sort_keys.push(keys);
        if let Value::Bool(b) = value {
            self.bool_and = Some(self.bool_and.unwrap_or(true) && *b);
            self.bool_or = Some(self.bool_or.unwrap_or(false) || *b);
        }
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
        if self.rows.len() >= REGISTERED_AGGREGATE_SPILL_ROWS {
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

    for entry in scored {
        let document = engine.get_document(table, entry.doc_id).unwrap_or_default();
        let ctx = uqa_sql::expr::EvalContext::new(Some(&document), params).with_engine(engine);
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
            let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(engine);
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
    for entry in scored {
        let document = engine.get_document(table, entry.doc_id).unwrap_or_default();
        let ctx = uqa_sql::expr::EvalContext::new(Some(&document), params).with_engine(engine);
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
    // Order the collected `values` by the captured sort keys when the
    // aggregate was annotated with ORDER BY (string_agg / array_agg /
    // percentile_*). This is a stable sort so equal keys preserve
    // insertion order, matching PostgreSQL.
    let ordered_values: Vec<Value> = if acc.sort_keys.iter().any(|k| !k.is_empty()) {
        let mut indexed: Vec<usize> = (0..acc.values.len()).collect();
        indexed.sort_by(|a, b| {
            let ak = &acc.sort_keys[*a];
            let bk = &acc.sort_keys[*b];
            for ((av, ad), (bv, _bd)) in ak.iter().zip(bk.iter()) {
                let cmp = av.cmp(bv);
                let cmp = if *ad { cmp.reverse() } else { cmp };
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            std::cmp::Ordering::Equal
        });
        indexed.into_iter().map(|i| acc.values[i].clone()).collect()
    } else {
        acc.values.clone()
    };

    let value = match lname.as_str() {
        "count" => Value::Int(acc.count as i64),
        "sum" => {
            if acc.count == 0 {
                Value::Null
            } else if acc.values.iter().all(|v| matches!(v, Value::Int(_))) {
                Value::Int(acc.sum as i64)
            } else {
                Value::Float(acc.sum)
            }
        }
        "avg" => {
            if acc.count == 0 {
                Value::Null
            } else {
                Value::Float(acc.sum / acc.count as f64)
            }
        }
        "min" => acc.min.clone().unwrap_or(Value::Null),
        "max" => acc.max.clone().unwrap_or(Value::Null),
        "string_agg" => {
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
                    Value::Bool(b) => Some(b.to_string()),
                    Value::Temporal(t) => Some(t.to_sql_string()),
                    _ => None,
                })
                .collect();
            Value::Str(parts.join(&sep))
        }
        "array_agg" => {
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            Value::List(ordered_values)
        }
        "json_object_agg" | "jsonb_object_agg" => {
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
            Value::Float(stddev_samp(&acc.values))
        }
        "stddev_pop" => {
            if acc.count == 0 {
                return Ok(Value::Null);
            }
            Value::Float(stddev_pop(&acc.values))
        }
        "variance" | "var_samp" => {
            if acc.count < 2 {
                return Ok(Value::Null);
            }
            Value::Float(variance_samp(&acc.values))
        }
        "var_pop" => {
            if acc.count == 0 {
                return Ok(Value::Null);
            }
            Value::Float(variance_pop(&acc.values))
        }
        "percentile_cont" => {
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            let frac = match args.first() {
                Some(Expr::Literal(Value::Float(f))) => *f,
                Some(Expr::Literal(Value::Int(n))) => *n as f64,
                _ => 0.5,
            };
            Value::Float(percentile_cont(&ordered_values, frac))
        }
        "percentile_disc" => {
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            let frac = match args.first() {
                Some(Expr::Literal(Value::Float(f))) => *f,
                Some(Expr::Literal(Value::Int(n))) => *n as f64,
                _ => 0.5,
            };
            percentile_disc(&ordered_values, frac)
        }
        "mode" => mode_value(&ordered_values),
        _ => Value::Null,
    };
    Ok(value)
}

fn aggregate_json_key(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
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
