//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar numeric operators share typed SQL overloads and existing arithmetic kernels.

use crate::ast::{ColumnType, Expr, FunctionBinding, NumericOperator};
use crate::type_resolution::numeric_operator_types;
use crate::{RowSchema, SQLError};
use uqa_core::Value;

use super::{eval, EvalContext, Result};

#[cfg(test)]
mod tests;

pub fn eval_numeric_operator(
    operator: NumericOperator,
    arguments: &[Value],
    types: &[Option<ColumnType>],
) -> Result<Value> {
    if arguments.len() != types.len() {
        return Err(SQLError::Internal(
            "operator operand/type count differs".into(),
        ));
    }
    let selected = numeric_operator_types(operator, types)?;
    let arguments = arguments
        .iter()
        .zip(types)
        .zip(&selected.arguments)
        .map(|((value, source), target)| {
            super::cast_value_from(
                value,
                &target.sql_name(),
                source.as_ref().map(ColumnType::sql_name).as_deref(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    if arguments.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(Value::Null);
    }
    let value = match operator {
        NumericOperator::Plus => arguments[0].clone(),
        NumericOperator::Modulo => super::scalar_dispatch::eval_scalar_function("mod", &arguments)?,
        NumericOperator::Power => {
            super::scalar_dispatch::eval_scalar_function("power", &arguments)?
        }
        NumericOperator::SquareRoot => {
            super::scalar_dispatch::eval_scalar_function("sqrt", &arguments)?
        }
        NumericOperator::CubeRoot => {
            super::scalar_dispatch::eval_scalar_function("cbrt", &arguments)?
        }
        NumericOperator::Absolute => {
            super::scalar_dispatch::eval_scalar_function("abs", &arguments)?
        }
    };
    // The shared integer carrier does not retain int2/int4 width. Enforce the selected operator's result width, including absolute-value overflow.
    super::cast_value(&value, &selected.result.sql_name())
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

pub(super) fn eval_bound_operator(
    operator: NumericOperator,
    binding: &FunctionBinding,
    arguments: &[Value],
) -> Result<Value> {
    let types = if binding.argument_types.is_empty() {
        arguments
            .iter()
            .map(crate::type_resolution::value_type)
            .collect()
    } else {
        binding
            .argument_types
            .iter()
            .map(|name| ColumnType::from_sql_name(name).map(Some))
            .collect::<Result<Vec<_>>>()?
    };
    eval_numeric_operator(operator, arguments, &types)
}
