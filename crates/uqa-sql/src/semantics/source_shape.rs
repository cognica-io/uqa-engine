//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source qualifiers, outer-join nullability, and subquery dependencies.

use super::from_qualifier_set;
use crate::plan::SourcePlan;
use crate::ScalarExpr;
use std::collections::BTreeSet;

/// Qualifiers whose rows can be synthesized as NULLs by an outer join cannot
/// receive an arbitrary WHERE predicate before that join. A predicate such as
/// `right.id IS NULL` accepts the synthesized row; pushing it into the right
/// scan first can remove a real match, manufacture a NULL-extended row, and
/// turn a non-result into a result. Keep predicates on these qualifiers above
/// the outer join unless a separate rewrite has first reduced it to an inner
/// join.
pub fn outer_join_nullable_qualifiers(from: &SourcePlan) -> BTreeSet<String> {
    let SourcePlan::Join {
        left,
        right,
        kind,
        alias,
        ..
    } = from
    else {
        return BTreeSet::new();
    };
    let left_nullable = outer_join_nullable_qualifiers(left);
    let right_nullable = outer_join_nullable_qualifiers(right);
    if let Some(alias) = alias {
        let nullable = matches!(
            kind,
            crate::ast::JoinKind::Left | crate::ast::JoinKind::Right | crate::ast::JoinKind::Full
        ) || !left_nullable.is_empty()
            || !right_nullable.is_empty();
        return if nullable {
            BTreeSet::from([alias.clone()])
        } else {
            BTreeSet::new()
        };
    }
    let mut nullable = left_nullable;
    nullable.extend(right_nullable);
    match kind {
        crate::ast::JoinKind::Left => nullable.extend(from_qualifier_set(right)),
        crate::ast::JoinKind::Right => nullable.extend(from_qualifier_set(left)),
        crate::ast::JoinKind::Full => {
            nullable.extend(from_qualifier_set(left));
            nullable.extend(from_qualifier_set(right));
        }
        crate::ast::JoinKind::Inner | crate::ast::JoinKind::Cross => {}
    }
    nullable
}

pub fn collect_from_qualifiers(from: &SourcePlan, out: &mut Vec<String>) {
    match from {
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if let Some(alias) = alias {
                out.push(alias.clone());
            } else {
                collect_from_qualifiers(left, out);
                collect_from_qualifiers(right, out);
            }
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => {
            if let Some(qualifier) = from.visible_qualifier() {
                out.push(qualifier.to_string());
            }
        }
    }
}

pub fn collect_subquery_ids(expression: &ScalarExpr, output: &mut BTreeSet<usize>) {
    match expression {
        ScalarExpr::ScalarSubquery(id) | ScalarExpr::Exists { subquery: id, .. } => {
            output.insert(*id);
        }
        ScalarExpr::InSubquery { expr, subquery, .. } => {
            collect_subquery_ids(expr, output);
            output.insert(*subquery);
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                collect_subquery_ids(item, output);
            }
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_subquery_ids(argument, output);
            }
            for order in order_by {
                collect_subquery_ids(&order.expr, output);
            }
            if let Some(filter) = filter {
                collect_subquery_ids(filter, output);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_subquery_ids(lhs, output);
            collect_subquery_ids(rhs, output);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => collect_subquery_ids(inner, output),
        ScalarExpr::Between { expr, low, high } => {
            collect_subquery_ids(expr, output);
            collect_subquery_ids(low, output);
            collect_subquery_ids(high, output);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_subquery_ids(expr, output);
            for item in list {
                collect_subquery_ids(item, output);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_subquery_ids(argument, output);
            }
            for partition in &spec.partition_by {
                collect_subquery_ids(partition, output);
            }
            for order in &spec.order_by {
                collect_subquery_ids(&order.expr, output);
            }
            if let Some(frame) = &spec.frame {
                collect_frame_bound_subquery_ids(&frame.start, output);
                collect_frame_bound_subquery_ids(&frame.end, output);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_subquery_ids(base, output);
            }
            for (condition, result) in when {
                collect_subquery_ids(condition, output);
                collect_subquery_ids(result, output);
            }
            if let Some(branch) = else_branch {
                collect_subquery_ids(branch, output);
            }
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
        | ScalarExpr::Param(_) => {}
    }
}

pub fn collect_frame_bound_subquery_ids(
    bound: &crate::ScalarFrameBound,
    output: &mut BTreeSet<usize>,
) {
    match bound {
        crate::ScalarFrameBound::Preceding(expression)
        | crate::ScalarFrameBound::Following(expression) => {
            collect_subquery_ids(expression, output);
        }
        crate::ScalarFrameBound::UnboundedPreceding
        | crate::ScalarFrameBound::UnboundedFollowing
        | crate::ScalarFrameBound::CurrentRow => {}
    }
}
