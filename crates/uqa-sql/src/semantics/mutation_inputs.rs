//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-independent mutation input shapes.
use crate::ScalarExpr;

/// A whole-column assignment whose expression needs no input row. Selecting it never evaluates the expression or its target coercion.
pub struct RowIndependentAssignment<'a> {
    pub column: &'a str,
    pub value: &'a ScalarExpr,
}

pub fn row_independent_assignments(
    assignments: &[crate::plan::AssignmentPlan],
) -> Option<Vec<RowIndependentAssignment<'_>>> {
    assignments
        .iter()
        .map(|assignment| {
            let crate::ast::AssignmentTargets::Single(target) = &assignment.target else {
                return None;
            };
            (target.is_whole_column() && expr_is_row_independent(&assignment.value)).then_some(
                RowIndependentAssignment {
                    column: &target.column,
                    value: &assignment.value,
                },
            )
        })
        .collect()
}

/// Borrow an equality lookup without evaluating its value before the caller selects the complete physical path.
pub fn point_lookup_input(filter: Option<&ScalarExpr>) -> Option<(&str, &ScalarExpr)> {
    let ScalarExpr::Binary {
        op: crate::ast::BinaryOp::Equal,
        lhs,
        rhs,
    } = filter?
    else {
        return None;
    };
    for (column, value) in [(lhs, rhs), (rhs, lhs)] {
        if let Some(column) = top_level_column(column) {
            if expr_is_row_independent(value) {
                return Some((column, value));
            }
        }
    }
    None
}

pub fn top_level_column(expr: &ScalarExpr) -> Option<&str> {
    match expr {
        ScalarExpr::Column(name) => Some(name),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column),
        _ => None,
    }
}

pub fn expr_is_row_independent(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_) => true,
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().all(expr_is_row_independent),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_is_row_independent(lhs) && expr_is_row_independent(rhs)
        }
        ScalarExpr::Not(inner) | ScalarExpr::UnaryMinus(inner) => expr_is_row_independent(inner),
        ScalarExpr::IsNull { expr, .. } => expr_is_row_independent(expr),
        ScalarExpr::Between { expr, low, high } => {
            expr_is_row_independent(expr)
                && expr_is_row_independent(low)
                && expr_is_row_independent(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_is_row_independent(expr) && list.iter().all(expr_is_row_independent)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().map_or(true, expr_is_row_independent)
                && when.iter().all(|(condition, result)| {
                    expr_is_row_independent(condition) && expr_is_row_independent(result)
                })
                && else_branch.as_deref().map_or(true, expr_is_row_independent)
        }
        ScalarExpr::Cast { expr, .. } | ScalarExpr::Collated { expr, .. } => {
            expr_is_row_independent(expr)
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Func { .. }
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
    }
}
