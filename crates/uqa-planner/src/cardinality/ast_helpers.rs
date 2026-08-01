//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-AST and scalar-value helpers for relational selectivity.

use super::{BinaryOp, ColumnStats, Expr, Value};

// ---------------------------------------------------------------------
// AST helpers (shared with `selectivity`)
// ---------------------------------------------------------------------

pub(super) fn column_of(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Column(c) => Some(c.as_str()),
        Expr::QualifiedColumn { column, .. } => Some(column.as_str()),
        _ => None,
    }
}

pub(super) fn literal_of(expr: &Expr) -> Option<&Value> {
    match expr {
        Expr::Literal(v) => Some(v),
        _ => None,
    }
}

pub(super) fn histogram_range_selectivity(
    stats: &ColumnStats,
    op: BinaryOp,
    value: &Value,
) -> Option<f64> {
    if stats.histogram.is_empty() {
        return None;
    }
    let bucket_count = (stats.histogram.len().saturating_sub(1)).max(1) as f64;
    let position = stats
        .histogram
        .iter()
        .position(|b| compare_values(b, value).is_ge())?;
    let frac = position as f64 / bucket_count;
    let s = match op {
        BinaryOp::Less | BinaryOp::LessEqual => frac,
        BinaryOp::Greater | BinaryOp::GreaterEqual => 1.0 - frac,
        _ => return None,
    };
    Some(s.clamp(0.0, 1.0))
}

pub(super) fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    a.cmp(b)
}

pub(super) fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}
