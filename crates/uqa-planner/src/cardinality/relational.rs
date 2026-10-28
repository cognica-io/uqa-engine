//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-AST predicate selectivity and row-count estimation.

use super::{
    column_of, histogram_range_selectivity, literal_of, BinaryOp, CardinalityEstimator, Expr,
    GraphStoreSampler, RelationStats, RelationStatsRef, Selectivity,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::{SQLError, ScalarExpr};

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
                    return Selectivity(stats.equality_selectivity_for(v)).clamp();
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
        self.scalar_selectivity_with_control(predicate, stats, &ProductionControl::uncontrolled())
            .expect("ordinary scalar costing retains best-effort constant conversion")
    }

    /// Estimate under the caller's allowance. Constant casts retain their produced storage through comparison; resource failures propagate instead of becoming heuristic estimates.
    pub fn scalar_selectivity_with_control(
        &self,
        predicate: &ScalarExpr,
        stats: &RelationStats,
        control: &ProductionControl<'_>,
    ) -> Result<Selectivity, SQLError> {
        self.scalar_selectivity_with_statistics(
            predicate,
            RelationStatsRef {
                row_count: stats.row_count,
                columns: &stats.columns,
            },
            control,
        )
    }

    pub(crate) fn scalar_selectivity_with_statistics(
        &self,
        predicate: &ScalarExpr,
        stats: RelationStatsRef<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Selectivity, SQLError> {
        control.check()?;
        if let Some(membership) = uqa_sql::semantics::membership_operands(predicate) {
            return self.scalar_membership_selectivity(membership, stats, control);
        }
        Ok(match predicate {
            ScalarExpr::And(parts) => {
                let mut selectivity = 1.0;
                for part in parts {
                    selectivity *= self
                        .scalar_selectivity_with_statistics(part, stats, control)?
                        .raw();
                }
                Selectivity(selectivity).clamp()
            }
            ScalarExpr::Or(parts) => {
                let mut anti = 1.0;
                for part in parts {
                    anti *= 1.0
                        - self
                            .scalar_selectivity_with_statistics(part, stats, control)?
                            .raw();
                }
                Selectivity(1.0 - anti).clamp()
            }
            ScalarExpr::Not(inner) => Selectivity(
                1.0 - self
                    .scalar_selectivity_with_statistics(inner, stats, control)?
                    .raw(),
            )
            .clamp(),
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
                self.scalar_binary_selectivity(*op, lhs, rhs, stats, control)?
            }
            ScalarExpr::Literal(Value::Bool(value))
            | ScalarExpr::TypedLiteral {
                value: Value::Bool(value),
                ..
            } => Selectivity(f64::from(*value)),
            ScalarExpr::Literal(Value::Null)
            | ScalarExpr::TypedLiteral {
                value: Value::Null, ..
            } => Selectivity(0.0),
            ScalarExpr::RetainedLiteral(literal) => match literal.value.resident_value() {
                Some(Value::Bool(value)) => Selectivity(f64::from(*value)),
                Some(Value::Null) => Selectivity(0.0),
                _ => Selectivity(self.default_selectivity).clamp(),
            },
            ScalarExpr::Between { .. } => Selectivity(self.range_selectivity).clamp(),
            ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            } => self.scalar_function_selectivity(name, binding.as_ref(), args, stats, control)?,
            _ => Selectivity(self.default_selectivity).clamp(),
        })
    }

    fn scalar_membership_selectivity(
        &self,
        membership: uqa_sql::semantics::MembershipOperands<'_>,
        stats: RelationStatsRef<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Selectivity, SQLError> {
        let selectivity = if let Some(column) =
            scalar_column(membership.value).and_then(|column| stats.column(column))
        {
            // Preserve Iterator::sum's floating-point identity, including an empty set.
            let mut selectivity = -0.0;
            for item in membership.items.iter() {
                control.check()?;
                let value = match item {
                    uqa_sql::semantics::MembershipItem::Expression(expression) => {
                        scalar_literal(expression, control)?
                    }
                    uqa_sql::semantics::MembershipItem::Constant(value) => {
                        Some(Literal::Borrowed(value))
                    }
                };
                selectivity += value.as_deref().map_or_else(
                    || column.equality_selectivity(),
                    |value| column.equality_selectivity_for(value),
                );
            }
            selectivity
        } else {
            self.default_selectivity * membership.items.len() as f64
        }
        .min(1.0);
        Ok(Selectivity(if membership.negated {
            1.0 - selectivity
        } else {
            selectivity
        })
        .clamp())
    }

    fn scalar_function_selectivity(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        args: &[ScalarExpr],
        stats: RelationStatsRef<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Selectivity, SQLError> {
        if let Some(uqa_sql::ast::FunctionDispatch::Pattern { operator, .. }) =
            binding.and_then(|binding| binding.dispatch)
        {
            if operator.is_like() {
                return Ok(Selectivity(if operator.negated() {
                    1.0 - self.like_selectivity
                } else {
                    self.like_selectivity
                })
                .clamp());
            }
        }
        if ["knn_match", "calibrated_vector_match"]
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            return Ok(args.get(2).and_then(scalar_positive_usize).map_or_else(
                || Selectivity(self.default_selectivity).clamp(),
                |k| top_k_selectivity(k, stats.row_count),
            ));
        }
        if [
            "fuse_bayesian_evidence",
            "fuse_log_odds",
            "pool_positive_evidence",
            "attention",
            "fuse_attention",
            "fuse_multihead",
            "learned_fusion",
            "fuse_learned",
        ]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            let mut support = -0.0;
            for signal in args.iter().filter(|signal| !scalar_named_argument(signal)) {
                support += self
                    .scalar_selectivity_with_statistics(signal, stats, control)?
                    .raw();
            }
            return Ok(Selectivity(support.min(1.0)).clamp());
        }
        Ok(Selectivity(self.default_selectivity).clamp())
    }

    fn scalar_binary_selectivity(
        &self,
        op: BinaryOp,
        lhs: &ScalarExpr,
        rhs: &ScalarExpr,
        stats: RelationStatsRef<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Selectivity, SQLError> {
        let column = scalar_column(lhs).or_else(|| scalar_column(rhs));
        let value = match scalar_literal(rhs, control)? {
            Some(value) => Some(value),
            None => scalar_literal(lhs, control)?,
        };
        let column_stats = column.and_then(|column| stats.column(column));
        if matches!(value.as_deref(), Some(Value::Null)) {
            return Ok(Selectivity(0.0));
        }
        Ok(match op {
            BinaryOp::Equal | BinaryOp::NotEqual => {
                let equal = column_stats.map_or(self.default_selectivity, |column| {
                    value.as_deref().map_or_else(
                        || column.equality_selectivity(),
                        |value| column.equality_selectivity_for(value),
                    )
                });
                let equal = Selectivity(equal).clamp().raw();
                Selectivity(if op == BinaryOp::NotEqual {
                    column_stats.map_or(1.0, super::ColumnStats::non_null_fraction) - equal
                } else {
                    equal
                })
                .clamp()
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                if let (Some(column), Some(value)) = (column_stats, value.as_deref()) {
                    if let Some(selectivity) = histogram_range_selectivity(column, op, value) {
                        return Ok(Selectivity(selectivity).clamp());
                    }
                }
                Selectivity(self.range_selectivity).clamp()
            }
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                Selectivity(self.default_selectivity).clamp()
            }
        })
    }
}

fn scalar_positive_usize(expression: &ScalarExpr) -> Option<usize> {
    let ScalarExpr::Literal(Value::Int(value)) = expression else {
        return None;
    };
    usize::try_from(*value).ok().filter(|value| *value > 0)
}

fn scalar_named_argument(expression: &ScalarExpr) -> bool {
    uqa_sql::scalar_call_argument(expression).is_ok_and(|argument| argument.name.is_some())
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

enum Literal<'a> {
    Borrowed(&'a Value),
    Produced(Produced<Value>),
}

impl std::ops::Deref for Literal<'_> {
    type Target = Value;
    fn deref(&self) -> &Value {
        match self {
            Self::Borrowed(value) => value,
            Self::Produced(value) => value,
        }
    }
}

fn scalar_literal<'a>(
    expression: &'a ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<Option<Literal<'a>>, SQLError> {
    control.check()?;
    Ok(match expression {
        ScalarExpr::Collated { expr, .. } => scalar_literal(expr, control)?,
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => {
            Some(Literal::Borrowed(value))
        }
        ScalarExpr::RetainedLiteral(literal) => {
            literal.value.resident_value().map(Literal::Borrowed)
        }
        ScalarExpr::Cast { expr, ty, .. } => {
            let Some(value) = scalar_literal(expr, control)? else {
                return Ok(None);
            };
            match uqa_sql::expr::cast_value_from_with_control(&value, ty, None, control) {
                Ok(value) => Some(Literal::Produced(value)),
                Err(error)
                    if control.budget().is_some()
                        && matches!(error.sqlstate(), Some("53200" | "57014")) =>
                {
                    return Err(error)
                }
                Err(_) => None,
            }
        }
        _ => None,
    })
}
