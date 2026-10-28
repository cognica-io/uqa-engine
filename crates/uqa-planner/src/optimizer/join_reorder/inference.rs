//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow equality decisions while their ordered union/intersection workspace retains its admission.

use super::{BinaryOp, ColumnOwners, ScalarExpr};
use crate::join_order::JoinPredicateRef;
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};
use uqa_sql::SQLError;

pub(super) type JoinPredicateKey<'a> = ((&'a str, &'a str), (&'a str, &'a str));

pub(super) type InferredPredicates<'p, 'c> =
    ProductionMap<'c, JoinPredicateKey<'p>, &'p ScalarExpr>;

pub(super) fn join_predicate<'p>(
    expression: &'p ScalarExpr,
    column_owners: &ColumnOwners<'p>,
) -> Option<JoinPredicateRef<'p>> {
    let ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    } = expression
    else {
        return None;
    };
    let (left_alias, left_field) = resolve_join_column(lhs, column_owners)?;
    let (right_alias, right_field) = resolve_join_column(rhs, column_owners)?;
    (left_alias != right_alias).then_some(JoinPredicateRef {
        left_alias,
        right_alias,
        left_field,
        right_field,
    })
}

fn resolve_join_column<'p>(
    expression: &'p ScalarExpr,
    owners: &ColumnOwners<'p>,
) -> Option<(&'p str, &'p str)> {
    match expression {
        ScalarExpr::QualifiedColumn { qualifier, column } if owners.contains_alias(qualifier) => {
            Some((qualifier, column.as_str()))
        }
        ScalarExpr::Column(column) => owners.resolve(column.as_str()),
        _ => None,
    }
}

fn predicate_key(predicate: JoinPredicateRef<'_>) -> JoinPredicateKey<'_> {
    let left = (predicate.left_alias, predicate.left_field);
    let right = (predicate.right_alias, predicate.right_field);
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

/// Conjunction keeps the first expression for each canonical equality; disjunction keeps the first branch's expression only when every subsequent branch implies that key. Results retain the original key order and operand orientation.
pub(super) fn implied_join_predicates<'p, 'c>(
    expression: &'p ScalarExpr,
    column_owners: &ColumnOwners<'p>,
    control: &ProductionControl<'c>,
) -> Result<InferredPredicates<'p, 'c>, SQLError> {
    control.check()?;
    if let Some(predicate) = join_predicate(expression, column_owners) {
        let mut output = ProductionMap::new(*control);
        output.insert_copy(predicate_key(predicate), expression)?;
        return Ok(output);
    }
    match expression {
        ScalarExpr::And(items) => {
            let mut output = ProductionMap::new(*control);
            for item in items {
                let child = implied_join_predicates(item, column_owners, control)?;
                for (key, predicate) in &child {
                    control.check()?;
                    if output.get(key).is_none() {
                        output.insert_copy(*key, *predicate)?;
                    }
                }
            }
            Ok(output)
        }
        ScalarExpr::Or(items) => {
            let Some((first, rest)) = items.split_first() else {
                return Ok(ProductionMap::new(*control));
            };
            let mut common = implied_join_predicates(first, column_owners, control)?;
            for item in rest {
                let branch = implied_join_predicates(item, column_owners, control)?;
                let mut removed = ProductionVec::new(*control);
                for (key, _) in &common {
                    control.check()?;
                    if branch.get(key).is_none() {
                        removed.push_copy(*key)?;
                    }
                }
                for key in removed.iter() {
                    control.check()?;
                    common.remove(key);
                }
                if common.is_empty() {
                    break;
                }
            }
            Ok(common)
        }
        _ => Ok(ProductionMap::new(*control)),
    }
}
