//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join conjunct analysis and structural side binding.

use super::ScalarExpr;
use uqa_execution::RowSchema;

pub(in crate::sql) fn join_conjuncts(expr: &ScalarExpr) -> Vec<&ScalarExpr> {
    match expr {
        ScalarExpr::And(items) => {
            let mut conjuncts = Vec::with_capacity(items.len());
            for item in items {
                conjuncts.extend(join_conjuncts(item));
            }
            conjuncts
        }
        _ => vec![expr],
    }
}

/// Determine the input side of two equality operands from structured schema identities. Planning does not synthesize a sample row, and punctuation in a quoted identifier is never interpreted as a relation boundary.
pub(in crate::sql) fn decide_join_sides<'a>(
    left: &RowSchema,
    right: &RowSchema,
    lhs: &'a ScalarExpr,
    rhs: &'a ScalarExpr,
) -> Option<(&'a ScalarExpr, &'a ScalarExpr)> {
    if expression_binds_to(lhs, left) && expression_binds_to(rhs, right) {
        return Some((lhs, rhs));
    }
    if expression_binds_to(rhs, left) && expression_binds_to(lhs, right) {
        return Some((rhs, lhs));
    }
    None
}

fn expression_binds_to(expression: &ScalarExpr, schema: &RowSchema) -> bool {
    let (valid, has_column) = expression_binding(expression, schema);
    valid && has_column
}

fn expression_binding(expression: &ScalarExpr, schema: &RowSchema) -> (bool, bool) {
    match expression {
        ScalarExpr::Column(column) => (schema.unqualified_position(column).is_some(), true),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            (schema.qualified_position(qualifier, column).is_some(), true)
        }
        ScalarExpr::Position(position) => (*position < schema.len(), true),
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => (true, false),
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => combine_bindings(
            args.iter()
                .chain(order_by.iter().map(|order| &order.expr))
                .chain(filter.as_deref()),
            schema,
        ),
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => combine_bindings(items, schema),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            combine_bindings([lhs.as_ref(), rhs.as_ref()], schema)
        }
        ScalarExpr::UnaryMinus(expr)
        | ScalarExpr::Not(expr)
        | ScalarExpr::IsNull { expr, .. }
        | ScalarExpr::Cast { expr, .. }
        | ScalarExpr::InSubquery { expr, .. } => expression_binding(expr, schema),
        ScalarExpr::Between { expr, low, high } => {
            combine_bindings([expr.as_ref(), low.as_ref(), high.as_ref()], schema)
        }
        ScalarExpr::InList { expr, list, .. } => {
            combine_bindings(std::iter::once(expr.as_ref()).chain(list), schema)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => combine_bindings(
            base.as_deref()
                .into_iter()
                .chain(
                    when.iter()
                        .flat_map(|(condition, value)| [condition, value]),
                )
                .chain(else_branch.as_deref()),
            schema,
        ),
        ScalarExpr::WindowCall { .. }
        | ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => (false, false),
    }
}

fn combine_bindings<'a>(
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    schema: &RowSchema,
) -> (bool, bool) {
    expressions
        .into_iter()
        .map(|expression| expression_binding(expression, schema))
        .fold((true, false), |(valid, has_column), binding| {
            (valid && binding.0, has_column || binding.1)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_execution::ColumnIdentity;

    #[test]
    fn join_side_binding_uses_structured_identities() {
        let left = RowSchema::with_identities(
            vec!["order.key".into()],
            vec![ColumnIdentity::qualified("left.alias", "order.key")],
            vec![None],
        );
        let right = RowSchema::with_qualified_types("right.alias", vec!["id".into()], vec![None]);
        let lhs = ScalarExpr::qualified_column("left.alias", "order.key");
        let rhs = ScalarExpr::qualified_column("right.alias", "id");
        assert_eq!(
            decide_join_sides(&left, &right, &lhs, &rhs),
            Some((&lhs, &rhs))
        );
    }

    #[test]
    fn ambiguous_unqualified_join_key_is_not_assigned_arbitrarily() {
        let left = RowSchema::with_identities(
            vec!["id".into(), "id".into()],
            vec![
                ColumnIdentity::qualified("left", "id"),
                ColumnIdentity::qualified("other", "id"),
            ],
            vec![None, None],
        );
        let right = RowSchema::with_qualified_types("right", vec!["id".into()], vec![None]);
        let lhs = ScalarExpr::Column("id".into());
        let rhs = ScalarExpr::qualified_column("right", "id");
        assert_eq!(decide_join_sides(&left, &right, &lhs, &rhs), None);
    }
}
