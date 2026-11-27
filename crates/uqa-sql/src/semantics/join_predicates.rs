//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join conjunct analysis and structural side binding.

use std::borrow::Cow;

use crate::RowSchema;
use crate::ScalarExpr;

/// Visit flattened conjunctions in their original order without allocating an intermediate reference tree.
pub fn visit_join_conjuncts_with_control<'a>(
    expression: &'a ScalarExpr,
    control: &uqa_core::memory::ProductionControl<'_>,
    visit: &mut impl FnMut(&'a ScalarExpr) -> Result<(), crate::SQLError>,
) -> Result<(), crate::SQLError> {
    super::expression_shape::visit_filter_parts_with_control(expression, control, visit)
}

/// Move an equality's selected operands to their own input scopes. Right-input positions change coordinates without copying either scalar tree.
pub fn bind_join_comparison_owned(
    left: &RowSchema,
    right: &RowSchema,
    comparison: crate::plan::OwnedScalarExpr,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<(crate::plan::OwnedScalarExpr, crate::plan::OwnedScalarExpr)>, crate::SQLError> {
    control.check()?;
    comparison.try_visit(&mut |_| {
        control.check()?;
        Ok::<_, crate::SQLError>(true)
    })?;
    let ScalarExpr::Binary { lhs, rhs, .. } = &*comparison else {
        panic!("join side binding requires a comparison");
    };
    let Some((selected, _)) = decide_join_sides(left, right, lhs, rhs) else {
        return Ok(None);
    };
    let reversed = !std::ptr::eq(selected, lhs.as_ref());
    let mut children = comparison.into_children();
    let lhs = children.next().expect("comparison left operand");
    let rhs = children.next().expect("comparison right operand");
    let (lhs, rhs) = if reversed { (rhs, lhs) } else { (lhs, rhs) };
    let rhs = if left.is_empty() {
        rhs
    } else {
        rhs.transform_nodes(control, &mut |_, expression| {
            if let ScalarExpr::Position(position) = &*expression {
                Ok(ScalarExpr::Position(position - left.len()).copy_for_rewrite(control)?)
            } else {
                Ok(expression)
            }
        })?
    };
    Ok(Some((lhs, rhs)))
}

pub fn join_conjuncts(expr: &ScalarExpr) -> Vec<&ScalarExpr> {
    super::expression_shape::flatten_and_filter_parts(expr)
}

/// Determine the input side of equality operands from structured identities and joined logical positions. Planning does not synthesize a sample row, and punctuation in a quoted identifier is never interpreted as a relation boundary.
pub fn decide_join_sides<'a>(
    left: &RowSchema,
    right: &RowSchema,
    lhs: &'a ScalarExpr,
    rhs: &'a ScalarExpr,
) -> Option<(&'a ScalarExpr, &'a ScalarExpr)> {
    if expression_binds(lhs, left, 0) && expression_binds(rhs, right, left.len()) {
        return Some((lhs, rhs));
    }
    if expression_binds(rhs, left, 0) && expression_binds(lhs, right, left.len()) {
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
    if offset == 0 || !expression.any_node(|part| matches!(part, ScalarExpr::Position(_))) {
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

fn expression_binds(expression: &ScalarExpr, schema: &RowSchema, offset: usize) -> bool {
    let mut has_column = false;
    let invalid = expression.any_node(|part| {
        let bound = match part {
            ScalarExpr::Column(name) => schema.unqualified_position(name).is_some(),
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                schema.qualified_position(qualifier, column).is_some()
            }
            ScalarExpr::Position(position) => position
                .checked_sub(offset)
                .is_some_and(|position| position < schema.len()),
            ScalarExpr::InternalColumn(column) => schema.internal_slot(*column).is_some(),
            ScalarExpr::WindowCall { .. }
            | ScalarExpr::Default
            | ScalarExpr::Star
            | ScalarExpr::QualifiedStar(_)
            | ScalarExpr::ScalarSubquery(_)
            | ScalarExpr::Exists { .. } => return true,
            _ => return false,
        };
        has_column = true;
        !bound
    });
    has_column && !invalid
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
