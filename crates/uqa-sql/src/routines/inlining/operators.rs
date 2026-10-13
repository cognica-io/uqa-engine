//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutability of the exact overload selected for a SQL binary operator.

use super::RoutineInliningContext;
use crate::{
    ast::{BinaryOp, FunctionBinding, FunctionDispatch, FunctionVolatility},
    ColumnType, RowSchema, SQLParam, ScalarExpr,
};

/// The SQL text/nonarray concatenation wrappers inline to textcat and the selected operand's text output cast.
pub(super) fn concatenation_volatility(
    context: &RoutineInliningContext<'_>,
    binding: Option<&FunctionBinding>,
    arguments: &[ScalarExpr],
    parameters: &[SQLParam],
    schema: &RowSchema,
) -> Option<FunctionVolatility> {
    let binding = binding.filter(|binding| binding.builtin)?;
    let FunctionDispatch::Concat { oid: Some(oid) } = binding.dispatch? else {
        return None;
    };
    let operator = crate::type_resolution::binary_operator_by_oid(i64::from(oid))?;
    let position = match (operator.name, operator.operand_types) {
        ("||", ["text", "anynonarray"]) => 1,
        ("||", ["anynonarray", "text"]) => 0,
        _ => return None,
    };
    let source = arguments.get(position).and_then(|argument| {
        crate::scalar_type_with_resolver(argument, schema, parameters, context.routines)
            .ok()
            .flatten()
    });
    Some(
        source
            .as_ref()
            .map_or(FunctionVolatility::Stable, |source| {
                crate::type_resolution::cast_volatility(source, &ColumnType::Text)
            }),
    )
}

pub(super) fn volatility(
    context: &RoutineInliningContext<'_>,
    op: BinaryOp,
    left: &ScalarExpr,
    right: &ScalarExpr,
    parameters: &[SQLParam],
    schema: &RowSchema,
) -> FunctionVolatility {
    let sources = [left, right].map(|expression| {
        crate::scalar_type_with_resolver(expression, schema, parameters, context.routines)
            .ok()
            .flatten()
    });
    // Caller columns have already been analyzed in a different row scope. No
    // built-in operator is volatile, so unknown types remain safely stable for
    // the repeated-argument test without inventing a bound operand identity.
    let [Some(left), Some(right)] = &sources else {
        return FunctionVolatility::Stable;
    };
    crate::type_resolution::binary_operator_volatility(op, Some(left), Some(right))
        .unwrap_or(FunctionVolatility::Stable)
}

pub(super) fn comparisons<'a>(
    context: &RoutineInliningContext<'_>,
    op: BinaryOp,
    expressions: impl IntoIterator<Item = (&'a ScalarExpr, &'a ScalarExpr)>,
    parameters: &[SQLParam],
    schema: &RowSchema,
) -> FunctionVolatility {
    if expressions.into_iter().any(|(left, right)| {
        volatility(context, op, left, right, parameters, schema) != FunctionVolatility::Immutable
    }) {
        FunctionVolatility::Stable
    } else {
        FunctionVolatility::Immutable
    }
}
