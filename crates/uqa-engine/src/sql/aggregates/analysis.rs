//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HAVING resolution, aggregate discovery, and group-row context.

use super::{Engine, ProjectionPlan, QueryBlockPlan, ResultRow, ScalarExpr, Value};

pub(in crate::sql) fn exprs_match(lhs: &ScalarExpr, rhs: &ScalarExpr) -> bool {
    match (lhs, rhs) {
        (ScalarExpr::Star, ScalarExpr::Star) => true,
        (ScalarExpr::Column(a), ScalarExpr::Column(b)) => a == b,
        (
            ScalarExpr::QualifiedColumn {
                qualifier: aq,
                column: ac,
                ..
            },
            ScalarExpr::QualifiedColumn {
                qualifier: bq,
                column: bc,
                ..
            },
        ) => aq == bq && ac == bc,
        (ScalarExpr::Column(c), ScalarExpr::QualifiedColumn { column, .. })
        | (ScalarExpr::QualifiedColumn { column, .. }, ScalarExpr::Column(c)) => c == column,
        (ScalarExpr::Literal(a), ScalarExpr::Literal(b)) => literals_equal(a, b),
        (ScalarExpr::Param(a), ScalarExpr::Param(b)) => a == b,
        (
            ScalarExpr::Func {
                name: an,
                args: aa,
                distinct: ad,
                order_by: ao,
                filter: af,
            },
            ScalarExpr::Func {
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
            ScalarExpr::Binary {
                op: ao,
                lhs: al,
                rhs: ar,
            },
            ScalarExpr::Binary {
                op: bo,
                lhs: bl,
                rhs: br,
            },
        ) => ao == bo && exprs_match(al, bl) && exprs_match(ar, br),
        (ScalarExpr::And(a), ScalarExpr::And(b)) | (ScalarExpr::Or(a), ScalarExpr::Or(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| exprs_match(x, y))
        }
        (ScalarExpr::Not(a), ScalarExpr::Not(b)) => exprs_match(a, b),
        (ScalarExpr::Cast { expr: a, ty: at }, ScalarExpr::Cast { expr: b, ty: bt }) => {
            at == bt && exprs_match(a, b)
        }
        _ => false,
    }
}

pub(in crate::sql) fn literals_equal(a: &Value, b: &Value) -> bool {
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

pub(in crate::sql) fn has_aggregate(engine: &Engine, projections: &[ProjectionPlan]) -> bool {
    projections
        .iter()
        .any(|p| contains_aggregate(engine, &p.expr))
}

pub(in crate::sql) fn is_aggregate(engine: &Engine, expr: &ScalarExpr) -> bool {
    matches!(expr, ScalarExpr::Func { name, .. } if matches!(
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

pub(in crate::sql) fn aggregate_exprs<'a>(
    engine: &Engine,
    projections: &'a [ProjectionPlan],
) -> Vec<&'a ScalarExpr> {
    let mut out = Vec::new();
    for projection in projections {
        collect_aggregate_exprs(engine, &projection.expr, &mut out);
    }
    out
}

/// Aggregate states needed by a query block, in accumulator order.
///
/// Projection aggregates remain first (including repeated expressions) because
/// projection rewriting consumes them positionally. Aggregates referenced only
/// by HAVING are appended once as hidden targets, matching `PostgreSQL`'s rule
/// that HAVING need not expose an aggregate in the SELECT list.
pub(in crate::sql) fn aggregate_targets<'a>(
    engine: &Engine,
    statement: &'a QueryBlockPlan,
) -> Vec<&'a ScalarExpr> {
    let mut targets = aggregate_exprs(engine, &statement.projections);
    if let Some(having) = statement.having.as_ref() {
        let mut hidden = Vec::new();
        collect_aggregate_exprs(engine, having, &mut hidden);
        for aggregate in hidden {
            if !targets
                .iter()
                .any(|existing| exprs_match(existing, aggregate))
            {
                targets.push(aggregate);
            }
        }
    }
    targets
}

pub(in crate::sql) fn collect_aggregate_exprs<'a>(
    engine: &Engine,
    expr: &'a ScalarExpr,
    out: &mut Vec<&'a ScalarExpr>,
) {
    if is_aggregate(engine, expr) {
        out.push(expr);
        return;
    }
    match expr {
        ScalarExpr::Func { args, filter, .. } => {
            for arg in args {
                collect_aggregate_exprs(engine, arg, out);
            }
            if let Some(filter) = filter.as_deref() {
                collect_aggregate_exprs(engine, filter, out);
            }
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                collect_aggregate_exprs(engine, item, out);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_aggregate_exprs(engine, lhs, out);
            collect_aggregate_exprs(engine, rhs, out);
        }
        ScalarExpr::Not(inner) | ScalarExpr::Cast { expr: inner, .. } => {
            collect_aggregate_exprs(engine, inner, out);
        }
        ScalarExpr::IsNull { expr, .. } => collect_aggregate_exprs(engine, expr, out),
        ScalarExpr::Between { expr, low, high } => {
            collect_aggregate_exprs(engine, expr, out);
            collect_aggregate_exprs(engine, low, out);
            collect_aggregate_exprs(engine, high, out);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_aggregate_exprs(engine, expr, out);
            for item in list {
                collect_aggregate_exprs(engine, item, out);
            }
        }
        ScalarExpr::Case {
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
        ScalarExpr::InSubquery { expr, .. } => collect_aggregate_exprs(engine, expr, out),
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
}

pub(in crate::sql) fn contains_aggregate(engine: &Engine, expr: &ScalarExpr) -> bool {
    let mut found = Vec::new();
    collect_aggregate_exprs(engine, expr, &mut found);
    !found.is_empty()
}

/// Collect the top-level column names an expression reads. Returns
/// `false` when the expression can reach arbitrary fields (`*`,
/// subqueries, window calls), in which case callers must materialise
/// whole documents.
pub(in crate::sql) fn expr_references_columns(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Star | ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. } => true,
        ScalarExpr::Func { args, filter, .. } => {
            args.iter().any(expr_references_columns)
                || filter.as_deref().is_some_and(expr_references_columns)
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_references_columns)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_references_columns(lhs) || expr_references_columns(rhs)
        }
        ScalarExpr::Not(inner) | ScalarExpr::Cast { expr: inner, .. } => {
            expr_references_columns(inner)
        }
        ScalarExpr::IsNull { expr, .. } => expr_references_columns(expr),
        ScalarExpr::Between { expr, low, high } => {
            expr_references_columns(expr)
                || expr_references_columns(low)
                || expr_references_columns(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_references_columns(expr) || list.iter().any(expr_references_columns)
        }
        ScalarExpr::WindowCall { args, .. } => args.iter().any(expr_references_columns),
        ScalarExpr::Case {
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
        ScalarExpr::InSubquery { expr, .. } => expr_references_columns(expr),
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Exists { .. } => true,
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => false,
    }
}

pub(in crate::sql) fn group_context_row(
    stmt: &QueryBlockPlan,
    group_values: &[Value],
) -> ResultRow {
    let mut row = ResultRow::new();
    for (expr, value) in stmt.group_by.iter().zip(group_values) {
        match expr {
            ScalarExpr::Column(column) => {
                row.insert(column.clone(), value.clone());
            }
            ScalarExpr::QualifiedColumn {
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
