//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retrieval classification, predicate priority, and access-path choice.

use super::{AccessPathPlan, BinaryOp, QueryBlockPlan, ScalarExpr, SourcePlan};

/// Put posting-list-compatible conjuncts before row residuals. The executor
/// can then build the smallest candidate set before touching documents.
pub(super) fn prioritize_access_predicates(expression: &mut ScalarExpr) {
    if let ScalarExpr::And(items) = expression {
        for item in items.iter_mut() {
            prioritize_access_predicates(item);
        }
        let mut access = Vec::with_capacity(items.len());
        let mut residual = Vec::new();
        for item in std::mem::take(items) {
            if operator_compatible(&item) {
                access.push(item);
            } else {
                residual.push(item);
            }
        }
        access.extend(residual);
        *items = access;
    }
}

pub(super) fn choose_access_path(block: &QueryBlockPlan) -> AccessPathPlan {
    if !matches!(block.from, Some(SourcePlan::Table { .. })) {
        return AccessPathPlan::Row;
    }
    let Some(predicate) = block.r#where.as_ref() else {
        return AccessPathPlan::Row;
    };
    if operator_compatible(predicate) {
        let score_limit_pushdown = block.limit.is_some()
            && !block.with_ties
            && root_score_retrieval(predicate)
            && !block.order_by.is_empty()
            && block.order_by.iter().all(|order| {
                order.descending
                    && matches!(
                        &order.expr,
                        ScalarExpr::Column(name)
                            | ScalarExpr::QualifiedColumn { column: name, .. }
                            if name == "_score"
                    )
            });
        AccessPathPlan::OperatorTree {
            score_limit_pushdown,
        }
    } else if contains_retrieval(predicate) {
        AccessPathPlan::Hybrid
    } else {
        AccessPathPlan::Row
    }
}

fn root_score_retrieval(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Func { name, .. }
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "text_match" | "bayesian_match" | "fts_match" | "bayesian_match_with_prior"
            )
    )
}

pub use uqa_sql::semantics::contains_retrieval;

fn operator_compatible(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            !items.is_empty() && items.iter().all(operator_compatible)
        }
        ScalarExpr::Not(inner) => operator_compatible(inner),
        ScalarExpr::Binary { op, lhs, rhs } => {
            matches!(
                op,
                BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual
            ) && scalar_operand(lhs)
                && scalar_operand(rhs)
        }
        ScalarExpr::IsNull { expr, .. } => scalar_operand(expr),
        ScalarExpr::Between { expr, low, high } => {
            scalar_operand(expr) && scalar_operand(low) && scalar_operand(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            scalar_operand(expr) && list.iter().all(scalar_operand)
        }
        ScalarExpr::Func { name, .. } => retrieval_function(name),
        _ => false,
    }
}

fn scalar_operand(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Column(_)
            | ScalarExpr::QualifiedColumn { .. }
            | ScalarExpr::Literal(_)
            | ScalarExpr::TypedLiteral { .. }
            | ScalarExpr::Param(_)
    )
}

pub use uqa_sql::semantics::retrieval_function;
