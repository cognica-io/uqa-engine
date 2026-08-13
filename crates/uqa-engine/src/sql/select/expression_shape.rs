//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression traversal, qualifier analysis, and safe qualification.

use super::{collect_from_qualifiers, BTreeSet, ScalarExpr, SourcePlan};

pub(in crate::sql) fn expr_contains_function(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Func { .. } | ScalarExpr::WindowCall { .. } => true,
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_contains_function)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_contains_function(lhs) || expr_contains_function(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_contains_function(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_contains_function(expr)
                || expr_contains_function(low)
                || expr_contains_function(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_contains_function(expr) || list.iter().any(expr_contains_function)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(expr_contains_function)
                || when.iter().any(|(condition, result)| {
                    expr_contains_function(condition) || expr_contains_function(result)
                })
                || else_branch.as_deref().is_some_and(expr_contains_function)
        }
        ScalarExpr::InSubquery { expr, .. } => expr_contains_function(expr),
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

pub(in crate::sql) fn flatten_and_filter_parts(expr: &ScalarExpr) -> Vec<&ScalarExpr> {
    match expr {
        ScalarExpr::And(items) => items.iter().flat_map(flatten_and_filter_parts).collect(),
        other => vec![other],
    }
}

pub(in crate::sql) fn from_qualifier_set(from: &SourcePlan) -> BTreeSet<String> {
    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    qualifiers.into_iter().collect()
}

pub(in crate::sql) fn expr_qualifiers(expr: &ScalarExpr) -> BTreeSet<String> {
    let mut qualifiers = BTreeSet::new();
    collect_expr_qualifiers(expr, &mut qualifiers);
    qualifiers
}

pub(in crate::sql) fn collect_expr_qualifiers(
    expr: &ScalarExpr,
    qualifiers: &mut BTreeSet<String>,
) {
    match expr {
        ScalarExpr::QualifiedColumn { qualifier, .. } => {
            qualifiers.insert(qualifier.clone());
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                collect_expr_qualifiers(item, qualifiers);
            }
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for arg in args {
                collect_expr_qualifiers(arg, qualifiers);
            }
            for order in order_by {
                collect_expr_qualifiers(&order.expr, qualifiers);
            }
            if let Some(filter) = filter.as_ref() {
                collect_expr_qualifiers(filter, qualifiers);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_expr_qualifiers(lhs, qualifiers);
            collect_expr_qualifiers(rhs, qualifiers);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            collect_expr_qualifiers(inner, qualifiers);
        }
        ScalarExpr::Between { expr, low, high } => {
            collect_expr_qualifiers(expr, qualifiers);
            collect_expr_qualifiers(low, qualifiers);
            collect_expr_qualifiers(high, qualifiers);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_expr_qualifiers(expr, qualifiers);
            for item in list {
                collect_expr_qualifiers(item, qualifiers);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for arg in args {
                collect_expr_qualifiers(arg, qualifiers);
            }
            for expr in &spec.partition_by {
                collect_expr_qualifiers(expr, qualifiers);
            }
            for order in &spec.order_by {
                collect_expr_qualifiers(&order.expr, qualifiers);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_ref() {
                collect_expr_qualifiers(base, qualifiers);
            }
            for (cond, result) in when {
                collect_expr_qualifiers(cond, qualifiers);
                collect_expr_qualifiers(result, qualifiers);
            }
            if let Some(else_branch) = else_branch.as_ref() {
                collect_expr_qualifiers(else_branch, qualifiers);
            }
        }
        ScalarExpr::InSubquery { expr, .. } => collect_expr_qualifiers(expr, qualifiers),
        ScalarExpr::Default
        | ScalarExpr::Column(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
}

pub(in crate::sql) fn expr_has_unqualified_column(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Column(_) => true,
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_has_unqualified_column)
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_has_unqualified_column)
                || order_by
                    .iter()
                    .any(|order| expr_has_unqualified_column(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|filter| expr_has_unqualified_column(filter))
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_has_unqualified_column(lhs) || expr_has_unqualified_column(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_has_unqualified_column(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_has_unqualified_column(expr)
                || expr_has_unqualified_column(low)
                || expr_has_unqualified_column(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_has_unqualified_column(expr) || list.iter().any(expr_has_unqualified_column)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(expr_has_unqualified_column)
                || spec.partition_by.iter().any(expr_has_unqualified_column)
                || spec
                    .order_by
                    .iter()
                    .any(|order| expr_has_unqualified_column(&order.expr))
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| expr_has_unqualified_column(expr))
                || when.iter().any(|(cond, result)| {
                    expr_has_unqualified_column(cond) || expr_has_unqualified_column(result)
                })
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_has_unqualified_column(expr))
        }
        ScalarExpr::InSubquery { expr, .. } => expr_has_unqualified_column(expr),
        ScalarExpr::Default
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

pub(in crate::sql) fn qualify_unqualified_columns(
    expr: &ScalarExpr,
    qualifier: &str,
) -> ScalarExpr {
    match expr {
        ScalarExpr::Column(column) => ScalarExpr::qualified_column(qualifier, column),
        ScalarExpr::Default
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star => expr.clone(),
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::And(items) => ScalarExpr::And(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::Or(items) => ScalarExpr::Or(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(qualify_unqualified_columns(lhs, qualifier)),
            rhs: Box::new(qualify_unqualified_columns(rhs, qualifier)),
        },
        ScalarExpr::Not(inner) => {
            ScalarExpr::Not(Box::new(qualify_unqualified_columns(inner, qualifier)))
        }
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            low: Box::new(qualify_unqualified_columns(low, qualifier)),
            high: Box::new(qualify_unqualified_columns(high, qualifier)),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            list: list
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
            negated: *negated,
        },
        ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            name: name.clone(),
            binding: binding.clone(),
            args: args
                .iter()
                .map(|arg| qualify_unqualified_columns(arg, qualifier))
                .collect(),
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_ref()
                .map(|filter| Box::new(qualify_unqualified_columns(filter, qualifier))),
        },
        ScalarExpr::WindowCall { name, args, spec } => ScalarExpr::WindowCall {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| qualify_unqualified_columns(arg, qualifier))
                .collect(),
            spec: spec.clone(),
        },
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base
                .as_ref()
                .map(|expr| Box::new(qualify_unqualified_columns(expr, qualifier))),
            when: when
                .iter()
                .map(|(cond, result)| {
                    (
                        qualify_unqualified_columns(cond, qualifier),
                        qualify_unqualified_columns(result, qualifier),
                    )
                })
                .collect(),
            else_branch: else_branch
                .as_ref()
                .map(|expr| Box::new(qualify_unqualified_columns(expr, qualifier))),
        },
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            ty: ty.clone(),
        },
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            subquery: *subquery,
            negated: *negated,
        },
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Exists { .. } => expr.clone(),
    }
}
