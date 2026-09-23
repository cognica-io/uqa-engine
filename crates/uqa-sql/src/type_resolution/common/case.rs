//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CASE type refinement shares controlled constant production and preserves semantic fallback and branch selection.

use super::{merge_value_types, ColumnType, SQLError, ScalarExpr, Value};
use uqa_core::memory::{Produced, ProductionControl};

pub(in crate::type_resolution) fn case_output_type_with_control(
    expression: &ScalarExpr,
    common: &ColumnType,
    infer: &mut super::super::call::InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    control.check()?;
    let ScalarExpr::Case {
        base,
        when,
        else_branch,
    } = expression
    else {
        return common.clone_with_control(control).map_err(Into::into);
    };
    let mut output = None;
    for (condition, value) in when {
        let condition = constant_case_condition(base.as_deref(), condition, infer, control)?;
        match condition.as_deref() {
            Some(Value::Bool(false) | Value::Null) => {}
            Some(Value::Bool(true)) => {
                include(Some(value), common, &mut output, infer, control)?;
                return output.map_or_else(
                    || {
                        common
                            .without_type_modifiers_with_control(control)
                            .map_err(Into::into)
                    },
                    Ok,
                );
            }
            _ => include(Some(value), common, &mut output, infer, control)?,
        }
    }
    include(else_branch.as_deref(), common, &mut output, infer, control)?;
    output.map_or_else(
        || {
            common
                .without_type_modifiers_with_control(control)
                .map_err(Into::into)
        },
        Ok,
    )
}

fn include(
    expression: Option<&ScalarExpr>,
    common: &ColumnType,
    output: &mut Option<Produced<ColumnType>>,
    infer: &mut super::super::call::InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let ty = expression
        .map(|expression| common_type_of(expression, infer, control))
        .transpose()?
        .flatten();
    let matching = match &ty {
        Some(ty) => {
            *ty.regtype_name_with_control(control)? == *common.regtype_name_with_control(control)?
        }
        None => false,
    };
    let ty = if matching {
        ty.expect("matching CASE type exists")
    } else {
        common.without_type_modifiers_with_control(control)?
    };
    *output = merge_value_types(output.take(), Some(ty), control)?;
    Ok(())
}

fn common_type_of(
    expression: &ScalarExpr,
    infer: &mut super::super::call::InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    if matches!(expression, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
        return Ok(None);
    }
    infer(expression)?
        .map(|ty| {
            let (ty, memory) = ty.into_parts();
            control.finish(ty, memory).map_err(Into::into)
        })
        .transpose()
}

fn constant_case_condition(
    base: Option<&ScalarExpr>,
    condition: &ScalarExpr,
    infer: &mut super::super::call::InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    let Some(base) = base else {
        return constant_value(condition, control);
    };
    let Some(left) = constant_value(base, control)? else {
        return Ok(None);
    };
    let Some(right) = constant_value(condition, control)? else {
        return Ok(None);
    };
    let Some(left_type) = semantic(common_type_of(base, infer, control), control)? else {
        return Ok(None);
    };
    let Some(right_type) = semantic(common_type_of(condition, infer, control), control)? else {
        return Ok(None);
    };
    let operand_type = match (left_type, right_type) {
        (Some(left), Some(right)) => {
            crate::type_resolution::equality_operand_type_with_control(&left, &right, control)
        }
        (Some(known), None) | (None, Some(known)) => known
            .without_type_modifiers_with_control(control)
            .map_err(Into::into),
        (None, None) => control
            .finish(ColumnType::Text, control.empty_reservation())
            .map_err(Into::into),
    };
    let Some(operand_type) = semantic(operand_type, control)? else {
        return Ok(None);
    };
    let ty = operand_type.sql_name_with_control(control)?;
    let Some(left) = semantic(
        crate::expr::cast_value_from_with_control(&left, &ty, None, control),
        control,
    )?
    else {
        return Ok(None);
    };
    let Some(right) = semantic(
        crate::expr::cast_value_from_with_control(&right, &ty, None, control),
        control,
    )?
    else {
        return Ok(None);
    };
    semantic(
        crate::expr::eval_binary_values_with_control(
            crate::ast::BinaryOp::Equal,
            &left,
            &right,
            control,
        ),
        control,
    )
}

fn constant_value(
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    control.check()?;
    match expression {
        ScalarExpr::Literal(value) => Ok(Some(control.copy_value(value)?)),
        ScalarExpr::Cast { expr, ty } => {
            let Some(value) = constant_value(expr, control)? else {
                return Ok(None);
            };
            semantic(
                crate::expr::cast_value_from_with_control(&value, ty, None, control),
                control,
            )
        }
        ScalarExpr::Binary { op, lhs, rhs } => {
            let Some(left) = constant_value(lhs, control)? else {
                return Ok(None);
            };
            let Some(right) = constant_value(rhs, control)? else {
                return Ok(None);
            };
            semantic(
                crate::expr::eval_binary_values_with_control(*op, &left, &right, control),
                control,
            )
        }
        _ => Ok(None),
    }
}

fn semantic<T>(
    result: Result<T, SQLError>,
    control: &ProductionControl<'_>,
) -> Result<Option<T>, SQLError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error)
            if control.budget().is_some()
                && matches!(error.sqlstate(), Some("53200" | "57014")) =>
        {
            Err(error)
        }
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests;
