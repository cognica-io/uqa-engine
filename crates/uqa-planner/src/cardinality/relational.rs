//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-AST predicate selectivity and row-count estimation.

use super::{
    column_of, histogram_range_selectivity, literal_of, BinaryOp, CardinalityEstimator, Expr,
    GraphStoreSampler, RelationStats, Selectivity,
};

impl CardinalityEstimator {
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
