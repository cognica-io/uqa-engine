//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated arguments for validated generated built-ins keep their existing payload owner through SQL dispatch.

use super::named::builtin_named_args;
use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    error::{Result, SQLError},
};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value,
};

/// Execute a built-in selected by generated-column validation. This boundary has no session, host-function or query callbacks; general runtime dispatch remains with `eval_function_call`. Argument values arrive already admitted by their producers.
pub fn eval_generated_function_call_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    arguments: Produced<Vec<(Option<String>, Value)>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    eval_builtin_function_call_with_control(
        name,
        binding,
        arguments,
        control,
        &super::EvalContext::new(None, &[]),
    )
}

/// Execute SQL-owned native built-ins with admitted arguments and the caller's catalog/comparison context. Session and user-function callbacks keep their separate execution boundaries.
pub fn eval_builtin_function_call_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    arguments: Produced<Vec<(Option<String>, Value)>>,
    control: &ProductionControl<'_>,
    context: &super::EvalContext<'_>,
) -> Result<Produced<Value>> {
    if let Some(error) = binding.and_then(|binding| binding.resolution_error.as_ref()) {
        return Err(error.sql_error());
    }
    control.check()?;
    if binding.is_some_and(|binding| !binding.builtin) {
        return Err(SQLError::Unsupported(
            "bound user function requires a logical engine session".into(),
        ));
    }
    if let (Some(binding), Some(hook)) = (binding, context.engine) {
        hook.require_builtin_execute(binding)?;
    }
    if let Some((binding, dispatch)) =
        binding.and_then(|binding| binding.dispatch.map(|dispatch| (binding, dispatch)))
    {
        if arguments.iter().any(|(name, _)| name.is_some()) {
            return Err(SQLError::Internal(format!(
                "bound {} expression retained a named argument",
                dispatch.label(),
            )));
        }
        let evaluated = MovedArguments::new(arguments, control)?;
        return eval_dispatched(binding, dispatch, &evaluated.values, control, context);
    }
    let name = binding.map_or(name, |binding| binding.name.as_str());
    let normalized =
        super::super::call_arguments::normalized_function_name_with_control(name, control)?;
    let name = normalized.as_ref();
    let value = if arguments.iter().any(|(name, _)| name.is_some()) {
        let Some(positional) = builtin_named_args(name, &arguments, control)? else {
            return Err(super::super::diagnostics::unknown_function_error(
                name, &arguments,
            ));
        };
        eval_scalar(name, &positional, control, context)?
    } else {
        let evaluated = MovedArguments::new(arguments, control)?;
        eval_scalar(name, &evaluated.values, control, context)?
    };
    if matches!(*value, Value::Int(_) | Value::Float(_)) {
        if let Some(binding) = binding {
            if let Some(ty) = crate::fixed_builtin_return_type_with_control(binding, control)? {
                if matches!(
                    *ty,
                    crate::ColumnType::SmallInteger
                        | crate::ColumnType::Integer
                        | crate::ColumnType::Real
                ) {
                    let name = ty.sql_name_with_control(control)?;
                    return super::super::cast_value_from_with_control(
                        &value, &name, None, control,
                    );
                }
            }
        }
    }
    Ok(value)
}

fn eval_dispatched(
    binding: &FunctionBinding,
    dispatch: FunctionDispatch,
    arguments: &[Value],
    control: &ProductionControl<'_>,
    context: &super::EvalContext<'_>,
) -> Result<Produced<Value>> {
    if let FunctionDispatch::Enum {
        operation,
        type_oid,
    } = dispatch
    {
        return super::super::enums::enum_function_value_with_control(
            context
                .engine
                .and_then(super::super::EngineHook::enum_labels),
            operation,
            type_oid,
            arguments,
            context.enum_comparison_state(),
            control,
        );
    }
    if let Some(result) = super::super::scalar_postgres::eval_comparison_with_context(
        dispatch, arguments, control, context,
    ) {
        return result;
    }
    super::super::builtin::eval_dispatched_builtin_with_control(
        binding, dispatch, arguments, control, context,
    )
}

fn eval_scalar(
    name: &str,
    arguments: &[Value],
    control: &ProductionControl<'_>,
    context: &super::EvalContext<'_>,
) -> Result<Produced<Value>> {
    if matches!(name, "coalesce" | "nullif" | "greatest" | "least") {
        return super::super::scalar_core::selection_with_context(
            name, arguments, control, context,
        );
    }
    super::super::scalar_dispatch::eval_generated_scalar_function(name, arguments, control)
}

/// Moving positional values must not make a second payload copy. The new vector's complete capacity is admitted by Core before movement; the original argument buffer and payload lease remain with this guard until dispatch finishes. Both vectors precede the shared lease on every failure path.
struct MovedArguments {
    values: Vec<Value>,
    remaining: std::vec::IntoIter<(Option<String>, Value)>,
    _memory: Option<MemoryReservation>,
}

impl MovedArguments {
    fn new(
        arguments: Produced<Vec<(Option<String>, Value)>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        let mut values = ProductionVec::new(*control);
        values.reserve(arguments.len())?;
        let (values, values_memory) = values.finish()?.into_parts();
        let (arguments, argument_memory) = arguments.into_parts();
        let mut output = Self {
            values,
            remaining: arguments.into_iter(),
            _memory: control.combine(values_memory, argument_memory),
        };
        for (_, value) in output.remaining.by_ref() {
            control.check()?;
            output.values.push(value);
        }
        control.check()?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests;
