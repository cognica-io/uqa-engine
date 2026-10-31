//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    coerce_routine_value, coerce_routine_value_from, context::RoutineExpressions, ArrayValue,
    CreateFunction, SQLError, Value,
};
use std::borrow::Cow;
use uqa_sql::ast::{FunctionParamMode, RoutineInvocationBinding, RoutineVariadicMode};
use uqa_sql::routines::invocation::descriptions::{
    RoutineCallDescription, RoutineConversionEnvironment,
};

#[derive(Clone, Copy)]
pub(crate) struct ArgumentDescriptions<'a> {
    pub description: &'a RoutineCallDescription,
    pub retained: super::invocation::context::RoutineCallRetention<'a>,
}

impl<'a> ArgumentDescriptions<'a> {
    fn source(&self, index: usize) -> Option<&'a Option<uqa_sql::ColumnType>> {
        self.description
            .is_current((self.retained.scope)().as_ref())
            .then(|| self.description.sources().get(index))
            .flatten()
    }

    fn environment<'b>(
        &'b self,
        expressions: &'b dyn RoutineExpressions,
    ) -> RoutineConversionEnvironment<'b> {
        RoutineConversionEnvironment {
            values: expressions,
            scope: self.retained.scope,
            control: self.retained.control,
        }
    }
}

pub fn materialize_arguments(
    expressions: &dyn RoutineExpressions,
    def: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    args: &[(Option<String>, Value)],
) -> Result<Vec<Value>, SQLError> {
    materialize_described_arguments(expressions, def, invocation, args, None)
}

pub(crate) fn materialize_described_arguments(
    expressions: &dyn RoutineExpressions,
    def: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    args: &[(Option<String>, Value)],
    descriptions: Option<ArgumentDescriptions<'_>>,
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
        let source = match descriptions.and_then(|saved| saved.source(argument_index)) {
            Some(source) => source.as_ref().map(Cow::Borrowed),
            None => invocation
                .argument_sources
                .get(argument_index)
                .and_then(Option::as_deref)
                .map(|name| expressions.column_type_name(name).map(Cow::Owned))
                .transpose()?,
        };
        let value = match descriptions {
            Some(saved) => saved.description.coerce_argument(
                argument_index,
                value,
                source.as_deref(),
                saved.environment(expressions),
            )?,
            None => coerce_routine_value_from(expressions, value, target, source.as_deref())?,
        };
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
        slots[parameter_index] = Some(match descriptions {
            Some(saved) => saved.description.coerce_parameter(
                parameter_index,
                &Value::Array(array),
                None,
                saved.environment(expressions),
            )?,
            None => coerce_routine_value(
                expressions,
                &Value::Array(array),
                &invocation.parameter_types[parameter_index],
            )?,
        });
    }
    materialize_defaults(expressions, def, invocation, slots, descriptions)
}

fn materialize_defaults(
    expressions: &dyn RoutineExpressions,
    def: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    mut slots: Vec<Option<Value>>,
    descriptions: Option<ArgumentDescriptions<'_>>,
) -> Result<Vec<Value>, SQLError> {
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
            match descriptions {
                Some(saved) => saved.description.coerce_parameter(
                    parameter_index,
                    &value,
                    source.as_deref(),
                    saved.environment(expressions),
                )?,
                None => coerce_routine_value_from(
                    expressions,
                    &value,
                    &invocation.parameter_types[parameter_index],
                    source.as_deref(),
                )?,
            }
        };
        bound.push(value);
    }
    Ok(bound)
}
