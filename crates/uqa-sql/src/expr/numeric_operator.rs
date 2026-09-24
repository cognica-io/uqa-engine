//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar numeric operators share typed SQL overloads and existing arithmetic kernels.

use crate::ast::{ColumnType, Expr, FunctionBinding, NumericOperator};
use crate::type_resolution::numeric_operator_types_with_control;
use crate::{RowSchema, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

use super::{eval, EvalContext, Result};

#[cfg(test)]
mod tests;

pub fn eval_numeric_operator(
    operator: NumericOperator,
    arguments: &[Value],
    types: &[Option<ColumnType>],
) -> Result<Value> {
    eval_numeric_operator_with_control(
        operator,
        arguments,
        types,
        &ProductionControl::uncontrolled(),
    )
    .map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary numeric result has no reservation")
    })
}

/// Evaluate the existing selected numeric overload while casts, type names and intermediate values remain owned by the original allowance.
pub fn eval_numeric_operator_with_control(
    operator: NumericOperator,
    arguments: &[Value],
    types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if arguments.len() != types.len() {
        return Err(SQLError::Internal(
            "operator operand/type count differs".into(),
        ));
    }
    let selected = numeric_operator_types_with_control(operator, types, control)?;
    let mut converted = ProductionVec::new(*control);
    converted.reserve(arguments.len())?;
    for ((value, source), target) in arguments.iter().zip(types).zip(&selected.arguments) {
        let target = target.sql_name_with_control(control)?;
        let source = source
            .as_ref()
            .map(|ty| ty.sql_name_with_control(control))
            .transpose()?;
        converted.push_produced(super::cast_value_from_with_control(
            value,
            &target,
            source.as_deref().map(String::as_str),
            control,
        )?)?;
    }
    let arguments = converted.finish()?;
    if arguments.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let value = if operator == NumericOperator::Plus {
        control.copy_value(&arguments[0])?
    } else {
        let name = match operator {
            NumericOperator::Modulo => "mod",
            NumericOperator::Power => "power",
            NumericOperator::SquareRoot => "sqrt",
            NumericOperator::CubeRoot => "cbrt",
            NumericOperator::Absolute => "abs",
            NumericOperator::Plus => unreachable!("unary plus copied its selected operand"),
        };
        super::scalar_core::eval_core_functions_with_control(name, &arguments, control)
            .or_else(|| {
                super::scalar_math::eval_math_functions_with_control(name, &arguments, control)
            })
            .expect("numeric syntax selects an existing numeric function")?
    };
    // The shared carrier does not retain int2/int4 width; the selected result cast enforces absolute-value overflow and real width.
    let target = selected.result.sql_name_with_control(control)?;
    super::cast_value_from_with_control(&value, &target, None, control)
}

pub(super) fn eval_ast_operator(
    operator: NumericOperator,
    binding: &FunctionBinding,
    arguments: &[Expr],
    context: &EvalContext<'_>,
) -> Result<Value> {
    let values = arguments
        .iter()
        .map(|arg| eval(arg, context))
        .collect::<Result<Vec<_>>>()?;
    let types = if binding.argument_types.is_empty() {
        arguments
            .iter()
            .zip(&values)
            .map(|(arg, value)| {
                let scalar = crate::plan::ExpressionPlan::lower(arg.clone()).scalar;
                let ty = crate::common_context_expression_type(
                    &scalar,
                    &RowSchema::default(),
                    context.params,
                    None,
                )?;
                Ok(ty.or_else(|| {
                    (!matches!(arg, Expr::Literal(Value::Str(_) | Value::Null)))
                        .then(|| crate::type_resolution::value_type(value))
                        .flatten()
                }))
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        binding
            .argument_types
            .iter()
            .map(|name| ColumnType::from_sql_name(name).map(Some))
            .collect::<Result<Vec<_>>>()?
    };
    eval_numeric_operator(operator, &values, &types)
}

pub(super) fn eval_bound_operator_with_control(
    operator: NumericOperator,
    binding: &FunctionBinding,
    arguments: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    let mut types = ProductionVec::new(*control);
    if binding.argument_types.is_empty() {
        types.reserve(arguments.len())?;
        for value in arguments {
            let ty = crate::type_resolution::value_type_with_control(value, control)?;
            let (ty, memory) = ty.map_or_else(
                || (None, control.empty_reservation()),
                |ty| {
                    let (ty, memory) = ty.into_parts();
                    (Some(ty), memory)
                },
            );
            types.push_produced(control.finish(ty, memory)?)?;
        }
    } else {
        types.reserve(binding.argument_types.len())?;
        for name in &binding.argument_types {
            let (ty, memory) = ColumnType::from_sql_name_with_control(name, control)?.into_parts();
            types.push_produced(control.finish(Some(ty), memory)?)?;
        }
    }
    let types = types.finish()?;
    eval_numeric_operator_with_control(operator, arguments, &types, control)
}

#[cfg(test)]
mod production_tests;
