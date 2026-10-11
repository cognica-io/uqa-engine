//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutability of the exact overload selected for a SQL binary operator.

use super::RoutineInliningContext;
use crate::{
    ast::{BinaryOp, FunctionVolatility},
    RowSchema, SQLParam, ScalarExpr,
};

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
