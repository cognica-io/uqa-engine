//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{ScalarExpr, ScalarOrder, WindowSlot};
use uqa_sql::ast::InternalRelationId;

pub(in crate::sql) fn expr_has_window(expr: &ScalarExpr) -> bool {
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
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().any(expr_has_window),
        ScalarExpr::Binary { lhs, rhs, .. } => expr_has_window(lhs) || expr_has_window(rhs),
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_has_window(inner),
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
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves partition peer and frame order"
)]
pub(super) fn rewrite_window_expr(
    expr: &ScalarExpr,
    slots: &mut Vec<WindowSlot>,
) -> (ScalarExpr, bool) {
    match expr {
        ScalarExpr::WindowCall { name, args, spec } => {
            let column = InternalRelationId::allocate().column(0);
            slots.push(WindowSlot {
                column,
                name: name.clone(),
                args: args.clone(),
                spec: spec.clone(),
            });
            (ScalarExpr::InternalColumn(column), true)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => {
            let (args, args_changed) = rewrite_window_exprs(args, slots);
            let (order_by, order_changed) = rewrite_window_order_by(order_by, slots);
            let (filter, filter_changed) = match filter {
                Some(expr) => {
                    let (expr, changed) = rewrite_window_expr(expr, slots);
                    (Some(Box::new(expr)), changed)
                }
                None => (None, false),
            };
            (
                ScalarExpr::Func {
                    name: name.clone(),
                    binding: binding.clone(),
                    args,
                    distinct: *distinct,
                    order_by,
                    filter,
                },
                args_changed || order_changed || filter_changed,
            )
        }
        ScalarExpr::Array(items) => {
            let (items, changed) = rewrite_window_exprs(items, slots);
            (ScalarExpr::Array(items), changed)
        }
        ScalarExpr::Row(items) => {
            let (items, changed) = rewrite_window_exprs(items, slots);
            (ScalarExpr::Row(items), changed)
        }
        ScalarExpr::Binary { op, lhs, rhs } => {
            let (lhs, lhs_changed) = rewrite_window_expr(lhs, slots);
            let (rhs, rhs_changed) = rewrite_window_expr(rhs, slots);
            (
                ScalarExpr::Binary {
                    op: *op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                lhs_changed || rhs_changed,
            )
        }
        ScalarExpr::Not(inner) => {
            let (inner, changed) = rewrite_window_expr(inner, slots);
            (ScalarExpr::Not(Box::new(inner)), changed)
        }
        ScalarExpr::UnaryMinus(inner) => {
            let (inner, changed) = rewrite_window_expr(inner, slots);
            (ScalarExpr::UnaryMinus(Box::new(inner)), changed)
        }
        ScalarExpr::And(items) => {
            let (items, changed) = rewrite_window_exprs(items, slots);
            (ScalarExpr::And(items), changed)
        }
        ScalarExpr::Or(items) => {
            let (items, changed) = rewrite_window_exprs(items, slots);
            (ScalarExpr::Or(items), changed)
        }
        ScalarExpr::IsNull { expr, negated } => {
            let (expr, changed) = rewrite_window_expr(expr, slots);
            (
                ScalarExpr::IsNull {
                    expr: Box::new(expr),
                    negated: *negated,
                },
                changed,
            )
        }
        ScalarExpr::Between { expr, low, high } => {
            let (expr, expr_changed) = rewrite_window_expr(expr, slots);
            let (low, low_changed) = rewrite_window_expr(low, slots);
            let (high, high_changed) = rewrite_window_expr(high, slots);
            (
                ScalarExpr::Between {
                    expr: Box::new(expr),
                    low: Box::new(low),
                    high: Box::new(high),
                },
                expr_changed || low_changed || high_changed,
            )
        }
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => {
            let (expr, expr_changed) = rewrite_window_expr(expr, slots);
            let (list, list_changed) = rewrite_window_exprs(list, slots);
            (
                ScalarExpr::InList {
                    expr: Box::new(expr),
                    list,
                    negated: *negated,
                },
                expr_changed || list_changed,
            )
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            let (base, base_changed) = match base {
                Some(expr) => {
                    let (expr, changed) = rewrite_window_expr(expr, slots);
                    (Some(Box::new(expr)), changed)
                }
                None => (None, false),
            };
            let mut changed = base_changed;
            let mut rewritten_when = Vec::with_capacity(when.len());
            for (cond, result) in when {
                let (cond, cond_changed) = rewrite_window_expr(cond, slots);
                let (result, result_changed) = rewrite_window_expr(result, slots);
                changed |= cond_changed || result_changed;
                rewritten_when.push((cond, result));
            }
            let (else_branch, else_changed) = match else_branch {
                Some(expr) => {
                    let (expr, changed) = rewrite_window_expr(expr, slots);
                    (Some(Box::new(expr)), changed)
                }
                None => (None, false),
            };
            (
                ScalarExpr::Case {
                    base,
                    when: rewritten_when,
                    else_branch,
                },
                changed || else_changed,
            )
        }
        ScalarExpr::Cast { expr, ty } => {
            let (expr, changed) = rewrite_window_expr(expr, slots);
            (
                ScalarExpr::Cast {
                    expr: Box::new(expr),
                    ty: ty.clone(),
                },
                changed,
            )
        }
        _ => (expr.clone(), false),
    }
}

fn rewrite_window_exprs(
    exprs: &[ScalarExpr],
    slots: &mut Vec<WindowSlot>,
) -> (Vec<ScalarExpr>, bool) {
    let mut changed = false;
    let rewritten = exprs
        .iter()
        .map(|expr| {
            let (expr, expr_changed) = rewrite_window_expr(expr, slots);
            changed |= expr_changed;
            expr
        })
        .collect();
    (rewritten, changed)
}

fn rewrite_window_order_by(
    order_by: &[ScalarOrder],
    slots: &mut Vec<WindowSlot>,
) -> (Vec<ScalarOrder>, bool) {
    let mut changed = false;
    let rewritten = order_by
        .iter()
        .map(|order| {
            let (expr, expr_changed) = rewrite_window_expr(&order.expr, slots);
            changed |= expr_changed;
            ScalarOrder {
                expr,
                descending: order.descending,
                nulls: order.nulls,
            }
        })
        .collect();
    (rewritten, changed)
}
