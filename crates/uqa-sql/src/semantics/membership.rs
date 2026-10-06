//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Membership operands shared by access selection, cardinality and key reads.

use crate::{ast::FunctionDispatch, ScalarExpr};
use uqa_core::Value;

/// An equality set or its SQL three-valued negation. The operands still require the consumer's ordinary safety and constant checks.
pub struct MembershipOperands<'a> {
    pub value: &'a ScalarExpr,
    pub items: MembershipItems<'a>,
    pub negated: bool,
}

/// Analyzed expressions or the borrowed values left after constant array folding.
#[derive(Clone, Copy)]
pub enum MembershipItems<'a> {
    Expressions(&'a [ScalarExpr]),
    Constants(&'a [Value]),
}

pub enum MembershipItem<'a> {
    Expression(&'a ScalarExpr),
    Constant(&'a Value),
}

impl<'a> MembershipItems<'a> {
    #[must_use]
    pub fn len(self) -> usize {
        match self {
            Self::Expressions(items) => items.len(),
            Self::Constants(items) => items.len(),
        }
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub fn iter(self) -> impl Iterator<Item = MembershipItem<'a>> {
        let (expressions, constants): (&[ScalarExpr], &[Value]) = match self {
            Self::Expressions(items) => (items, &[]),
            Self::Constants(items) => (&[], items),
        };
        expressions
            .iter()
            .map(MembershipItem::Expression)
            .chain(constants.iter().map(MembershipItem::Constant))
    }
}

/// Borrow an IN list or its analyzed scalar-array comparison without dropping conversions. Empty ALL arrays stay scalar because they also accept a NULL left operand.
#[must_use]
pub fn membership_operands(expression: &ScalarExpr) -> Option<MembershipOperands<'_>> {
    match expression {
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => Some(MembershipOperands {
            value: expr,
            items: MembershipItems::Expressions(list),
            negated: *negated,
        }),
        ScalarExpr::Func {
            binding: Some(binding),
            args,
            ..
        } => {
            let [value, array, ScalarExpr::Literal(Value::Str(operator))] = args.as_slice() else {
                return None;
            };
            let items = match array {
                ScalarExpr::Array(items) => MembershipItems::Expressions(items),
                ScalarExpr::Literal(Value::Array(array))
                | ScalarExpr::TypedLiteral {
                    value: Value::Array(array),
                    ..
                } if array.dimensions().len() <= 1 => MembershipItems::Constants(array.elements()),
                _ => return None,
            };
            let negated = match (binding.dispatch, operator.as_str()) {
                (Some(FunctionDispatch::AnyOperator), "=") => false,
                (Some(FunctionDispatch::AllOperator), "<>") if !items.is_empty() => true,
                _ => return None,
            };
            Some(MembershipOperands {
                value,
                items,
                negated,
            })
        }
        _ => None,
    }
}

/// Query-local columns participate in IN-list partitioning and window placement; an enclosing query's columns do not. A declared schema without physical scopes treats every column as local.
pub(crate) fn references_current_row(
    expression: &ScalarExpr,
    schema: Option<&crate::RowSchema>,
) -> bool {
    let mut found = false;
    expression.visit(&mut |node| {
        found |= match node {
            ScalarExpr::Column(column) => {
                schema.is_none_or(|schema| schema.resolves_local_column(None, column))
            }
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                schema.is_none_or(|schema| schema.resolves_local_column(Some(qualifier), column))
            }
            ScalarExpr::Position(position) => schema.is_none_or(|schema| {
                schema
                    .slot(*position)
                    .is_some_and(|slot| schema.slot_is_local(slot))
            }),
            ScalarExpr::InternalColumn(column) => schema.is_none_or(|schema| {
                schema
                    .internal_slot(*column)
                    .is_some_and(|slot| schema.slot_is_local(slot))
            }),
            ScalarExpr::QualifiedStar(qualifier) => schema.is_none_or(|schema| {
                schema
                    .identities()
                    .iter()
                    .any(|identity| identity.qualifier() == Some(qualifier.as_str()))
            }),
            _ => false,
        };
    });
    found
}
