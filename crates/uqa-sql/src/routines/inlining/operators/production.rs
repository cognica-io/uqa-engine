//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inlining volatility observes selected operand types through admitted producers.

use super::{
    BinaryOp, FunctionBinding, FunctionDispatch, FunctionVolatility, RoutineInliningContext,
    RowSchema, SQLParam, ScalarExpr,
};
use crate::{ColumnType, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

pub(in crate::routines::inlining) fn optional_type(
    context: &RoutineInliningContext<'_>,
    expression: &ScalarExpr,
    parameters: &[SQLParam],
    schema: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match crate::scalar_type_with_resolver_and_control(
        expression,
        schema,
        parameters,
        Some(context.routines),
        control,
    ) {
        Ok(ty) => Ok(ty),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(None),
    }
}

pub(in crate::routines::inlining) fn concatenation_volatility_with_control(
    context: &RoutineInliningContext<'_>,
    binding: Option<&FunctionBinding>,
    arguments: &[ScalarExpr],
    parameters: &[SQLParam],
    schema: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Option<FunctionVolatility>, SQLError> {
    control.check()?;
    let Some(binding) = binding.filter(|binding| binding.builtin) else {
        return Ok(None);
    };
    let Some(FunctionDispatch::Concat { oid: Some(oid) }) = binding.dispatch else {
        return Ok(None);
    };
    let Some(operator) = crate::type_resolution::binary_operator_by_oid(i64::from(oid)) else {
        return Ok(None);
    };
    let position = match (operator.name, operator.operand_types) {
        ("||", ["text", "anynonarray"]) => 1,
        ("||", ["anynonarray", "text"]) => 0,
        _ => return Ok(None),
    };
    let source = arguments
        .get(position)
        .map(|argument| optional_type(context, argument, parameters, schema, control))
        .transpose()?
        .flatten();
    Ok(Some(match source {
        Some(source) => crate::type_resolution::cast_volatility_with_control(
            &source,
            &ColumnType::Text,
            control,
        )?,
        None => FunctionVolatility::Stable,
    }))
}

pub(in crate::routines::inlining) fn volatility_with_control(
    context: &RoutineInliningContext<'_>,
    op: BinaryOp,
    left: &ScalarExpr,
    right: &ScalarExpr,
    parameters: &[SQLParam],
    schema: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<FunctionVolatility, SQLError> {
    let left = optional_type(context, left, parameters, schema, control)?;
    let right = optional_type(context, right, parameters, schema, control)?;
    let (Some(left), Some(right)) = (left, right) else {
        return Ok(FunctionVolatility::Stable);
    };
    match crate::type_resolution::binary_operator_volatility_with_control(
        op,
        Some(&left),
        Some(&right),
        control,
    ) {
        Ok(volatility) => Ok(volatility),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(FunctionVolatility::Stable),
    }
}

pub(in crate::routines::inlining) fn comparisons_with_control<'a>(
    context: &RoutineInliningContext<'_>,
    op: BinaryOp,
    expressions: impl IntoIterator<Item = (&'a ScalarExpr, &'a ScalarExpr)>,
    parameters: &[SQLParam],
    schema: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<FunctionVolatility, SQLError> {
    control.check()?;
    for (left, right) in expressions {
        if volatility_with_control(context, op, left, right, parameters, schema, control)?
            != FunctionVolatility::Immutable
        {
            return Ok(FunctionVolatility::Stable);
        }
    }
    Ok(FunctionVolatility::Immutable)
}
