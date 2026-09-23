//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime operand type spelling and integer widths share SQL's existing inference rules.

use crate::{
    ast::{BinaryOp, ColumnType},
    expr::{integer_width_for_literal, integer_width_for_type, IntegerWidth},
    schema::ScalarTypeSchema,
    SQLError, SQLParam, ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

pub fn scalar_operand_type_name(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
) -> Option<String> {
    scalar_operand_type_name_with_control(
        expression,
        schema,
        params,
        &ProductionControl::uncontrolled(),
    )
    .ok()
    .flatten()
    .map(|name| {
        name.into_uncontrolled()
            .expect("ordinary operand type name")
    })
}

/// Preserve explicit binding and domain precedence while admitting inferred types and their emitted names. Optional semantic inference failures remain absent; resource failures propagate to the original caller.
pub fn scalar_operand_type_name_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<String>>, SQLError> {
    control.check()?;
    let name = match expression {
        ScalarExpr::TypedLiteral {
            bound_type: Some(ty),
            ..
        } => {
            return Ok(Some(
                base_type(ty, control)?.sql_name_with_control(control)?,
            ));
        }
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } if binding
            .invocation
            .as_ref()
            .is_some_and(|invocation| invocation.return_type.is_some()) =>
        {
            binding
                .invocation
                .as_ref()
                .and_then(|invocation| invocation.return_type.as_deref())
        }
        ScalarExpr::Cast { ty, .. } | ScalarExpr::TypedLiteral { ty, .. } => Some(ty.as_str()),
        ScalarExpr::UnaryMinus(inner) => {
            return scalar_operand_type_name_with_control(inner, schema, params, control);
        }
        ScalarExpr::Literal(Value::Int(value)) if i32::try_from(*value).is_ok() => Some("integer"),
        ScalarExpr::Literal(Value::Int(_)) => Some("bigint"),
        ScalarExpr::Literal(Value::Bytes(_)) => Some("bytea"),
        ScalarExpr::Literal(Value::Str(_) | Value::FixedChar(_)) => return Ok(None),
        _ => {
            let Some(ty) = inferred_type(expression, schema, params, control)? else {
                return Ok(None);
            };
            return Ok(Some(
                base_type(&ty, control)?.sql_name_with_control(control)?,
            ));
        }
    };
    name.map(|name| control.copy_text(name).map_err(Into::into))
        .transpose()
}

pub fn scalar_integer_operation_width(
    lhs: &ScalarExpr,
    rhs: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
) -> Option<IntegerWidth> {
    scalar_integer_operation_width_with_control(
        lhs,
        rhs,
        schema,
        params,
        &ProductionControl::uncontrolled(),
    )
    .ok()
    .flatten()
}

/// Select the existing arithmetic width from borrowed IR and schema metadata without constructing an untracked inferred type or name.
pub fn scalar_integer_operation_width_with_control(
    lhs: &ScalarExpr,
    rhs: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<IntegerWidth>, SQLError> {
    let Some(left) = operand_width(lhs, schema, params, control)? else {
        return Ok(None);
    };
    Ok(operand_width(rhs, schema, params, control)?.map(|right| left.max(right)))
}

fn operand_width(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<IntegerWidth>, SQLError> {
    if let Some(width) = explicit_width(expression, control)? {
        return Ok(Some(width));
    }
    let Some(ty) = inferred_type(expression, schema, params, control)? else {
        return Ok(None);
    };
    let name = base_type(&ty, control)?.sql_name_with_control(control)?;
    Ok(integer_width_for_type(&name))
}

fn explicit_width(
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<Option<IntegerWidth>, SQLError> {
    control.check()?;
    Ok(match expression {
        ScalarExpr::Literal(Value::Int(value)) => Some(integer_width_for_literal(*value)),
        ScalarExpr::TypedLiteral {
            bound_type: Some(ty),
            ..
        } => integer_width_for_type(&base_type(ty, control)?.sql_name_with_control(control)?),
        ScalarExpr::Cast { ty, .. } | ScalarExpr::TypedLiteral { ty, .. } => {
            integer_width_for_type(ty)
        }
        ScalarExpr::UnaryMinus(inner) => explicit_width(inner, control)?,
        ScalarExpr::Binary {
            op: BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide,
            lhs,
            rhs,
        } => match explicit_width(lhs, control)? {
            Some(left) => explicit_width(rhs, control)?.map(|right| left.max(right)),
            None => None,
        },
        _ => None,
    })
}

fn base_type<'a>(
    mut ty: &'a ColumnType,
    control: &ProductionControl<'_>,
) -> Result<&'a ColumnType, SQLError> {
    while let ColumnType::Domain { base, .. } = ty {
        control.check()?;
        ty = base;
    }
    Ok(ty)
}

fn inferred_type(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match super::scalar_type_with_control(expression, schema, params, control) {
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(None),
        result => result,
    }
}

#[cfg(test)]
mod tests;
