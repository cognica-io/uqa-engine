//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    coerce_routine_value, coerce_routine_value_from, context::RoutineExpressions, ArrayValue,
    CreateFunction, SQLError, Value,
};
use uqa_sql::ast::{FunctionParamMode, RoutineInvocationBinding, RoutineVariadicMode};

pub fn materialize_arguments(
    expressions: &dyn RoutineExpressions,
    def: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    args: &[(Option<String>, Value)],
) -> Result<Vec<Value>, SQLError> {
    if invocation.argument_positions.len() != args.len()
        || invocation.argument_targets.len() != args.len()
        || invocation.parameter_types.len() != def.params.len()
    {
        return Err(SQLError::Internal(format!(
            "routine `{}` has an inconsistent invocation binding",
            def.name
        )));
    }
    let expanded_parameter = match invocation.variadic_mode {
        RoutineVariadicMode::Expanded { parameter_index } => Some(parameter_index),
        RoutineVariadicMode::None | RoutineVariadicMode::Explicit { .. } => None,
    };
    let mut slots = vec![None; def.params.len()];
    let mut expanded_values = Vec::new();
    for (argument_index, ((_, value), parameter_index)) in
        args.iter().zip(&invocation.argument_positions).enumerate()
    {
        let target = &invocation.argument_targets[argument_index];
        let source = invocation
            .argument_sources
            .get(argument_index)
            .and_then(Option::as_deref)
            .map(|name| expressions.column_type_name(name))
            .transpose()?;
        let value = coerce_routine_value_from(expressions, value, target, source.as_ref())?;
        if Some(*parameter_index) == expanded_parameter {
            expanded_values.push(value);
        } else if slots[*parameter_index].replace(value).is_some() {
            return Err(SQLError::Internal(format!(
                "routine `{}` bound more than one argument to parameter {}",
                def.name,
                parameter_index + 1
            )));
        }
    }
    if let Some(parameter_index) = expanded_parameter {
        let array = ArrayValue::try_new(expanded_values).ok_or_else(|| {
            SQLError::Internal(format!(
                "routine `{}` could not materialize its variadic array",
                def.name
            ))
        })?;
        slots[parameter_index] = Some(coerce_routine_value(
            expressions,
            &Value::Array(array),
            &invocation.parameter_types[parameter_index],
        )?);
    }
    let mut bound = Vec::with_capacity(def.call_arity());
    for (parameter_index, parameter) in def.params.iter().enumerate() {
        let takes_argument = match parameter.mode {
            FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic => true,
            FunctionParamMode::Out => def.is_procedure,
            FunctionParamMode::Table => false,
        };
        if !takes_argument {
            continue;
        }
        let value = if let Some(value) = slots[parameter_index].take() {
            value
        } else {
            let default = parameter.default.as_ref().ok_or_else(|| {
                SQLError::Internal(format!(
                    "routine `{}` lost required parameter {} after overload resolution",
                    def.name,
                    parameter_index + 1
                ))
            })?;
            let (value, source) = expressions.evaluate_with_type(default)?;
            coerce_routine_value_from(
                expressions,
                &value,
                &invocation.parameter_types[parameter_index],
                source.as_ref(),
            )?
        };
        bound.push(value);
    }
    Ok(bound)
}
