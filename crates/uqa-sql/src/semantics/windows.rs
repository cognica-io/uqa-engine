//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window expression classification.

use crate::ScalarExpr;

/// Dedicated window functions select their kernels directly, independently of the aggregate registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinWindowFunction {
    RowNumber,
    Rank,
    DenseRank,
    PercentRank,
    CumeDist,
    Ntile,
    Lag,
    Lead,
    FirstValue,
    LastValue,
    NthValue,
}

impl BuiltinWindowFunction {
    pub fn from_dispatch_name(name: &str) -> Option<Self> {
        Some(match name {
            "row_number" => Self::RowNumber,
            "rank" => Self::Rank,
            "dense_rank" => Self::DenseRank,
            "percent_rank" => Self::PercentRank,
            "cume_dist" => Self::CumeDist,
            "ntile" => Self::Ntile,
            "lag" => Self::Lag,
            "lead" => Self::Lead,
            "first_value" => Self::FirstValue,
            "last_value" => Self::LastValue,
            "nth_value" => Self::NthValue,
            _ => return None,
        })
    }

    pub(crate) fn accepts(self, arity: usize) -> bool {
        match self {
            Self::RowNumber | Self::Rank | Self::DenseRank | Self::PercentRank | Self::CumeDist => {
                arity == 0
            }
            Self::Ntile | Self::FirstValue | Self::LastValue => arity == 1,
            Self::NthValue => arity == 2,
            Self::Lag | Self::Lead => (1..=3).contains(&arity),
        }
    }
}

pub fn expr_has_window(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::WindowCall { .. } => true,
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_has_window)
                || order_by.iter().any(|order| expr_has_window(&order.expr))
                || filter.as_ref().is_some_and(|expr| expr_has_window(expr))
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().any(expr_has_window),
        ScalarExpr::Binary { lhs, rhs, .. } => expr_has_window(lhs) || expr_has_window(rhs),
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => expr_has_window(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_has_window(expr) || expr_has_window(low) || expr_has_window(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_has_window(expr) || list.iter().any(expr_has_window)
        }
        ScalarExpr::Case {
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
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
    }
}
