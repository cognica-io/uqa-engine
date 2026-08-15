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
use uqa_core::Value;
use uqa_execution::ScalarExpr;

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

    /// Estimate a predicate already lowered to the physical scalar IR. This
    /// keeps join ordering sensitive to local WHERE filters without converting
    /// the optimized plan back into parser AST nodes.
    pub fn scalar_selectivity(&self, predicate: &ScalarExpr, stats: &RelationStats) -> Selectivity {
        match predicate {
            ScalarExpr::And(parts) => Selectivity(
                parts
                    .iter()
                    .map(|part| self.scalar_selectivity(part, stats).raw())
                    .product(),
            )
            .clamp(),
            ScalarExpr::Or(parts) => {
                let anti = parts.iter().fold(1.0, |anti, part| {
                    anti * (1.0 - self.scalar_selectivity(part, stats).raw())
                });
                Selectivity(1.0 - anti).clamp()
            }
            ScalarExpr::Not(inner) => {
                Selectivity(1.0 - self.scalar_selectivity(inner, stats).raw()).clamp()
            }
            ScalarExpr::IsNull { expr, negated } => {
                let null_fraction = scalar_column(expr)
                    .and_then(|column| stats.column(column))
                    .map_or(0.05, |column| {
                        if stats.row_count == 0 {
                            0.0
                        } else {
                            column.null_count as f64 / stats.row_count as f64
                        }
                    });
                Selectivity(if *negated {
                    1.0 - null_fraction
                } else {
                    null_fraction
                })
                .clamp()
            }
            ScalarExpr::Binary { op, lhs, rhs } => {
                self.scalar_binary_selectivity(*op, lhs, rhs, stats)
            }
            ScalarExpr::InList {
                expr,
                list,
                negated,
            } => {
                let selectivity = scalar_column(expr)
                    .and_then(|column| stats.column(column))
                    .map_or(self.default_selectivity * list.len() as f64, |column| {
                        list.iter()
                            .filter_map(scalar_literal)
                            .map(|value| {
                                column
                                    .matches_mcv(&value)
                                    .unwrap_or_else(|| column.equality_selectivity())
                            })
                            .sum()
                    })
                    .min(1.0);
                Selectivity(if *negated {
                    1.0 - selectivity
                } else {
                    selectivity
                })
                .clamp()
            }
            ScalarExpr::Between { .. } => Selectivity(self.range_selectivity).clamp(),
            ScalarExpr::Func { name, .. } if name.eq_ignore_ascii_case("like") => {
                Selectivity(self.like_selectivity).clamp()
            }
            ScalarExpr::Func { name, args, .. } => {
                self.scalar_function_selectivity(name, args, stats)
            }
            ScalarExpr::Literal(Value::Bool(value)) => Selectivity(if *value { 1.0 } else { 0.0 }),
            _ => Selectivity(self.default_selectivity).clamp(),
        }
    }

    fn scalar_function_selectivity(
        &self,
        name: &str,
        args: &[ScalarExpr],
        stats: &RelationStats,
    ) -> Selectivity {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "knn_match" | "calibrated_vector_match" => {
                args.get(2).and_then(scalar_positive_usize).map_or_else(
                    || Selectivity(self.default_selectivity).clamp(),
                    |k| top_k_selectivity(k, stats.row_count),
                )
            }
            "fuse_bayesian_evidence"
            | "fuse_log_odds"
            | "pool_positive_evidence"
            | "attention"
            | "fuse_attention"
            | "fuse_multihead"
            | "learned_fusion"
            | "fuse_learned" => {
                let support = args
                    .iter()
                    .filter(|signal| !scalar_named_argument(signal))
                    .map(|signal| self.scalar_selectivity(signal, stats).raw())
                    .sum::<f64>();
                Selectivity(support.min(1.0)).clamp()
            }
            _ => Selectivity(self.default_selectivity).clamp(),
        }
    }

    fn scalar_binary_selectivity(
        &self,
        op: BinaryOp,
        lhs: &ScalarExpr,
        rhs: &ScalarExpr,
        stats: &RelationStats,
    ) -> Selectivity {
        let column = scalar_column(lhs).or_else(|| scalar_column(rhs));
        let value = scalar_literal(rhs).or_else(|| scalar_literal(lhs));
        let column_stats = column.and_then(|column| stats.column(column));
        match op {
            BinaryOp::Equal => {
                if let (Some(column), Some(value)) = (column_stats, value.as_ref()) {
                    return Selectivity(
                        column
                            .matches_mcv(value)
                            .unwrap_or_else(|| column.equality_selectivity()),
                    )
                    .clamp();
                }
                Selectivity(self.default_selectivity).clamp()
            }
            BinaryOp::NotEqual => {
                let equal = self
                    .scalar_binary_selectivity(BinaryOp::Equal, lhs, rhs, stats)
                    .raw();
                Selectivity(1.0 - equal).clamp()
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                if let (Some(column), Some(value)) = (column_stats, value.as_ref()) {
                    if let Some(selectivity) = histogram_range_selectivity(column, op, value) {
                        return Selectivity(selectivity).clamp();
                    }
                }
                Selectivity(self.range_selectivity).clamp()
            }
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                Selectivity(self.default_selectivity).clamp()
            }
        }
    }
}

fn scalar_positive_usize(expression: &ScalarExpr) -> Option<usize> {
    let ScalarExpr::Literal(Value::Int(value)) = expression else {
        return None;
    };
    usize::try_from(*value).ok().filter(|value| *value > 0)
}

fn scalar_named_argument(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Func { name, args, .. } if name == "__named_arg" && args.len() == 2
    )
}

fn top_k_selectivity(k: usize, row_count: u64) -> Selectivity {
    if row_count == 0 {
        return Selectivity(0.0);
    }
    let rows = u64::try_from(k).unwrap_or(u64::MAX).min(row_count);
    Selectivity(rows as f64 / row_count as f64).clamp()
}

fn scalar_column(expression: &ScalarExpr) -> Option<&str> {
    match expression {
        ScalarExpr::Column(column) => Some(column),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column),
        _ => None,
    }
}

fn scalar_literal(expression: &ScalarExpr) -> Option<Value> {
    match expression {
        ScalarExpr::Literal(value) => Some(value.clone()),
        ScalarExpr::Cast { expr, ty } => {
            let value = scalar_literal(expr)?;
            uqa_sql::expr::cast_value(&value, ty).ok()
        }
        _ => None,
    }
}
