//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cardinality estimation.
//!
//! Mirrors `uqa/planner/cardinality.py`. We track per-column
//! [`ColumnStats`] (distinct count, null count, min/max, MCV +
//! frequency, equi-depth histogram) and combine them through a
//! [`CardinalityEstimator`] that turns predicate selectivities into
//! row count estimates.
//!
//! The estimator produces *unitless selectivity* values in `[0, 1]`
//! plus row counts; downstream code (e.g. the join enumerator)
//! multiplies through to derive plan cardinalities.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, Expr};

#[derive(Debug, Clone, Default)]
pub struct ColumnStats {
    pub distinct_count: u64,
    pub null_count: u64,
    pub min_value: Option<Value>,
    pub max_value: Option<Value>,
    pub row_count: u64,
    /// Equi-depth histogram bucket boundaries, sorted ascending.
    /// `b+1` boundaries describe `b` buckets.
    pub histogram: Vec<Value>,
    /// Most-common values, descending by frequency.
    pub mcv_values: Vec<Value>,
    pub mcv_frequencies: Vec<f64>,
}

impl ColumnStats {
    /// Default selectivity of an equality predicate over this column.
    pub fn equality_selectivity(&self) -> f64 {
        if self.distinct_count == 0 {
            1.0
        } else {
            1.0 / self.distinct_count as f64
        }
    }

    pub fn matches_mcv(&self, value: &Value) -> Option<f64> {
        for (mcv, freq) in self.mcv_values.iter().zip(self.mcv_frequencies.iter()) {
            if mcv == value {
                return Some(*freq);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct RelationStats {
    pub row_count: u64,
    pub columns: BTreeMap<String, ColumnStats>,
}

impl RelationStats {
    pub fn new(row_count: u64) -> Self {
        Self {
            row_count,
            columns: BTreeMap::new(),
        }
    }

    pub fn with_column(mut self, name: impl Into<String>, stats: ColumnStats) -> Self {
        self.columns.insert(name.into(), stats);
        self
    }

    pub fn column(&self, name: &str) -> Option<&ColumnStats> {
        self.columns.get(name)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Selectivity(pub f64);

impl Selectivity {
    pub fn clamp(self) -> Self {
        Self(self.0.clamp(0.0, 1.0))
    }

    pub fn raw(self) -> f64 {
        self.0
    }
}

#[derive(Debug, Clone, Default)]
pub struct CardinalityEstimator {
    /// Default selectivity for an unknown predicate.
    pub default_selectivity: f64,
    /// Default selectivity for a `LIKE 'foo%'` style prefix match.
    pub like_selectivity: f64,
    /// Default selectivity for an inequality predicate with no histogram.
    pub range_selectivity: f64,
}

impl CardinalityEstimator {
    pub fn new() -> Self {
        Self {
            default_selectivity: 0.1,
            like_selectivity: 0.05,
            range_selectivity: 0.3,
        }
    }

    /// Estimate the selectivity of `predicate` against `stats`. Best
    /// effort: unknown shapes fall back on
    /// [`Self::default_selectivity`].
    pub fn selectivity(&self, predicate: &Expr, stats: &RelationStats) -> Selectivity {
        match predicate {
            Expr::And(parts) => {
                let mut s = 1.0;
                for p in parts {
                    s *= self.selectivity(p, stats).raw();
                }
                Selectivity(s).clamp()
            }
            Expr::Or(parts) => {
                let mut anti = 1.0;
                for p in parts {
                    anti *= 1.0 - self.selectivity(p, stats).raw();
                }
                Selectivity(1.0 - anti).clamp()
            }
            Expr::Not(inner) => Selectivity(1.0 - self.selectivity(inner, stats).raw()).clamp(),
            Expr::IsNull { expr, negated } => {
                let col = column_of(expr);
                let null_frac = col
                    .and_then(|c| stats.column(c))
                    .map(|s| {
                        if stats.row_count == 0 {
                            0.0
                        } else {
                            s.null_count as f64 / stats.row_count as f64
                        }
                    })
                    .unwrap_or(0.05);
                let s = if *negated { 1.0 - null_frac } else { null_frac };
                Selectivity(s).clamp()
            }
            Expr::Binary { op, lhs, rhs } => self.binary_selectivity(*op, lhs, rhs, stats),
            Expr::InList { list, negated, .. } => {
                let s = (list.len() as f64) * self.default_selectivity;
                let s = s.min(1.0);
                Selectivity(if *negated { 1.0 - s } else { s }).clamp()
            }
            Expr::Between { .. } => Selectivity(self.range_selectivity).clamp(),
            _ => Selectivity(self.default_selectivity).clamp(),
        }
    }

    fn binary_selectivity(
        &self,
        op: BinaryOp,
        lhs: &Expr,
        rhs: &Expr,
        stats: &RelationStats,
    ) -> Selectivity {
        let col = column_of(lhs).or_else(|| column_of(rhs));
        let constant = literal_of(rhs).or_else(|| literal_of(lhs));
        let col_stats = col.and_then(|name| stats.column(name));
        let value = constant;
        match op {
            BinaryOp::Equal => {
                if let (Some(stats), Some(v)) = (col_stats, value) {
                    if let Some(freq) = stats.matches_mcv(v) {
                        return Selectivity(freq).clamp();
                    }
                    return Selectivity(stats.equality_selectivity()).clamp();
                }
                Selectivity(self.default_selectivity).clamp()
            }
            BinaryOp::NotEqual => {
                let eq = self
                    .binary_selectivity(BinaryOp::Equal, lhs, rhs, stats)
                    .raw();
                Selectivity(1.0 - eq).clamp()
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                if let (Some(stats), Some(v)) = (col_stats, value) {
                    if let Some(s) = histogram_range_selectivity(stats, op, v) {
                        return Selectivity(s).clamp();
                    }
                }
                Selectivity(self.range_selectivity).clamp()
            }
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                Selectivity(self.default_selectivity).clamp()
            }
        }
    }

    /// Estimated row count after applying `predicate` to `stats`.
    pub fn estimate_rows(&self, predicate: &Expr, stats: &RelationStats) -> u64 {
        let s = self.selectivity(predicate, stats).raw();
        ((stats.row_count as f64) * s).round() as u64
    }
}

fn column_of(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Column(c) => Some(c.as_str()),
        Expr::QualifiedColumn { column, .. } => Some(column.as_str()),
        _ => None,
    }
}

fn literal_of(expr: &Expr) -> Option<&Value> {
    match expr {
        Expr::Literal(v) => Some(v),
        _ => None,
    }
}

fn histogram_range_selectivity(stats: &ColumnStats, op: BinaryOp, value: &Value) -> Option<f64> {
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

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Value::Null, Value::Null) => Equal,
        (Value::Null, _) => Less,
        (_, Value::Null) => Greater,
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str) -> Expr {
        Expr::Column(name.into())
    }

    fn eq(name: &str, v: i64) -> Expr {
        Expr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(col(name)),
            rhs: Box::new(Expr::Literal(Value::Int(v))),
        }
    }

    #[test]
    fn equality_uses_distinct_count() {
        let stats = RelationStats::new(1000).with_column(
            "user_id",
            ColumnStats {
                distinct_count: 250,
                row_count: 1000,
                ..Default::default()
            },
        );
        let est = CardinalityEstimator::new();
        let sel = est.selectivity(&eq("user_id", 7), &stats).raw();
        assert!((sel - (1.0 / 250.0)).abs() < 1e-9);
        assert_eq!(est.estimate_rows(&eq("user_id", 7), &stats), 4);
    }

    #[test]
    fn and_selectivity_multiplies() {
        let stats = RelationStats::new(1000).with_column(
            "uid",
            ColumnStats {
                distinct_count: 100,
                row_count: 1000,
                ..Default::default()
            },
        );
        let est = CardinalityEstimator::new();
        let pred = Expr::And(vec![eq("uid", 1), eq("uid", 2)]);
        let sel = est.selectivity(&pred, &stats).raw();
        assert!((sel - (0.01 * 0.01)).abs() < 1e-9);
    }

    #[test]
    fn or_selectivity_uses_inclusion_exclusion() {
        let stats = RelationStats::new(1000).with_column(
            "uid",
            ColumnStats {
                distinct_count: 10,
                row_count: 1000,
                ..Default::default()
            },
        );
        let est = CardinalityEstimator::new();
        let pred = Expr::Or(vec![eq("uid", 1), eq("uid", 2)]);
        let sel = est.selectivity(&pred, &stats).raw();
        // 1 - (1 - 0.1)^2 = 0.19
        assert!((sel - 0.19).abs() < 1e-9);
    }
}
