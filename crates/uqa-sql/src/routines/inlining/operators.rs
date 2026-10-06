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
    let Ok([target_left, target_right, _]) =
        crate::type_resolution::binary_operator_types(op, Some(left), Some(right))
    else {
        return FunctionVolatility::Stable;
    };
    if crate::type_resolution::cast_volatility(left, &target_left) != FunctionVolatility::Immutable
        || crate::type_resolution::cast_volatility(right, &target_right)
            != FunctionVolatility::Immutable
    {
        return FunctionVolatility::Stable;
    }
    let identity =
        crate::type_resolution::binary_operator_catalog_entry(op, [&target_left, &target_right]);
    // PostgreSQL 18.4 pg_proc: these are the stable implementations in the
    // existing binary-operator catalog. All its other implementations are
    // immutable and all have proisstrict=true and procost=1.
    if identity.is_ok_and(|entry| {
        matches!(entry.function_oid,
        1189 | 1190 | 2549 | 2351..=2356 | 2377..=2382 | 2520..=2525 | 2527..=2532)
    }) {
        FunctionVolatility::Stable
    } else {
        FunctionVolatility::Immutable
    }
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
