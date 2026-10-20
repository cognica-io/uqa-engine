//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join conjunct analysis and structural side binding.

use std::borrow::Cow;

use crate::RowSchema;
use crate::ScalarExpr;

pub fn join_conjuncts(expr: &ScalarExpr) -> Vec<&ScalarExpr> {
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

/// Determine the input side of equality operands from structured identities and joined logical positions. Planning does not synthesize a sample row, and punctuation in a quoted identifier is never interpreted as a relation boundary.
pub fn decide_join_sides<'a>(
    left: &RowSchema,
    right: &RowSchema,
    lhs: &'a ScalarExpr,
    rhs: &'a ScalarExpr,
) -> Option<(&'a ScalarExpr, &'a ScalarExpr)> {
    let binds = |expression, schema, offset| {
        let (valid, has_column) = expression_binding(expression, schema, offset);
        valid && has_column
    };
    if binds(lhs, left, 0) && binds(rhs, right, left.len()) {
        return Some((lhs, rhs));
    }
    if binds(rhs, left, 0) && binds(lhs, right, left.len()) {
        return Some((rhs, lhs));
    }
    None
}

/// Rebase selected right-input positions for independent key evaluation, borrowing expressions whose coordinates already match their own input.
pub fn bind_join_sides<'a>(
    left: &RowSchema,
    right: &RowSchema,
    lhs: &'a ScalarExpr,
    rhs: &'a ScalarExpr,
) -> Option<(Cow<'a, ScalarExpr>, Cow<'a, ScalarExpr>)> {
    let (lhs, rhs) = decide_join_sides(left, right, lhs, rhs)?;
    Some((Cow::Borrowed(lhs), local_join_key(rhs, left.len())))
}

fn local_join_key(expression: &ScalarExpr, offset: usize) -> Cow<'_, ScalarExpr> {
    let mut has_position = false;
    if offset != 0 {
        expression.visit(&mut |part| has_position |= matches!(part, ScalarExpr::Position(_)));
    }
    if !has_position {
        return Cow::Borrowed(expression);
    }
    let mut bound = expression.clone();
    crate::plan::rewrite_scalar_expression(&mut bound, &mut |part| {
        if let ScalarExpr::Position(position) = part {
            *position -= offset;
        }
    });
    Cow::Owned(bound)
}

fn expression_binding(expression: &ScalarExpr, schema: &RowSchema, offset: usize) -> (bool, bool) {
    match expression {
        ScalarExpr::Column(column) => (schema.unqualified_position(column).is_some(), true),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            (schema.qualified_position(qualifier, column).is_some(), true)
        }
        ScalarExpr::Position(position) => (
            position
                .checked_sub(offset)
                .is_some_and(|position| position < schema.len()),
            true,
        ),
        ScalarExpr::InternalColumn(column) => (schema.internal_slot(*column).is_some(), true),
        ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_) => (true, false),
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
            offset,
        ),
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => combine_bindings(items, schema, offset),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            combine_bindings([lhs.as_ref(), rhs.as_ref()], schema, offset)
        }
        ScalarExpr::UnaryMinus(expr)
        | ScalarExpr::Not(expr)
        | ScalarExpr::IsNull { expr, .. }
        | ScalarExpr::Cast { expr, .. }
        | ScalarExpr::Collated { expr, .. }
        | ScalarExpr::InSubquery { expr, .. } => expression_binding(expr, schema, offset),
        ScalarExpr::Between { expr, low, high } => {
            combine_bindings([expr.as_ref(), low.as_ref(), high.as_ref()], schema, offset)
        }
        ScalarExpr::InList { expr, list, .. } => {
            combine_bindings(std::iter::once(expr.as_ref()).chain(list), schema, offset)
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
            offset,
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
    offset: usize,
) -> (bool, bool) {
    expressions
        .into_iter()
        .map(|expression| expression_binding(expression, schema, offset))
        .fold((true, false), |(valid, has_column), binding| {
            (valid && binding.0, has_column || binding.1)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ColumnIdentity;

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
    fn joined_positions_are_rebased_only_to_their_own_input() {
        let left = RowSchema::new(vec!["l0".into(), "l1".into()]);
        let right = RowSchema::new(vec!["r0".into()]);
        let lhs = ScalarExpr::Position(1);
        let rhs = ScalarExpr::Cast {
            implicit: true,
            expr: Box::new(ScalarExpr::Position(2)),
            ty: "bigint".into(),
        };
        let expected = ScalarExpr::Cast {
            implicit: true,
            expr: Box::new(ScalarExpr::Position(0)),
            ty: "bigint".into(),
        };
        for (a, b) in [(&lhs, &rhs), (&rhs, &lhs)] {
            let (a, b) = bind_join_sides(&left, &right, a, b).unwrap();
            assert_eq!(a.as_ref(), &lhs);
            assert_eq!(b.as_ref(), &expected);
        }
        for other in [
            ScalarExpr::Position(0),
            ScalarExpr::Position(3),
            ScalarExpr::Literal(uqa_core::Value::Int(1)),
        ] {
            assert!(decide_join_sides(&left, &right, &lhs, &other).is_none());
        }
        let mixed = ScalarExpr::Binary {
            op: crate::ast::BinaryOp::Add,
            lhs: Box::new(lhs.clone()),
            rhs: Box::new(rhs.clone()),
        };
        assert!(decide_join_sides(&left, &right, &mixed, &rhs).is_none());
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
