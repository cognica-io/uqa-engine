//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated arguments for validated generated built-ins keep their existing payload owner through SQL dispatch.

use super::{arguments::MovedArguments, named::builtin_named_args};
use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    error::{Result, SQLError},
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum BuiltinEvaluation {
    Session,
    Admitted,
}

/// Execute the selected built-in with session semantics and ordinary argument ownership.
pub fn eval_bound_builtin_function_call(
    binding: &FunctionBinding,
    arguments: Vec<(Option<String>, Value)>,
    context: &super::EvalContext<'_>,
) -> Result<Value> {
    let control = ProductionControl::uncontrolled();
    evaluate(
        &binding.name,
        Some(binding),
        control.finish(arguments, None)?,
        &control,
        context,
        BuiltinEvaluation::Session,
    )
    .map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary bound builtin result")
    })
}

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
    evaluate(
        name,
        binding,
        arguments,
        control,
        context,
        BuiltinEvaluation::Admitted,
    )
}

fn evaluate(
    name: &str,
    binding: Option<&FunctionBinding>,
    arguments: Produced<Vec<(Option<String>, Value)>>,
    control: &ProductionControl<'_>,
    context: &super::EvalContext<'_>,
    evaluation: BuiltinEvaluation,
) -> Result<Produced<Value>> {
    if let Some(error) = binding.and_then(|binding| binding.resolution_error.as_ref()) {
        return Err(error.sql_error());
    }
    control.check()?;
    if evaluation == BuiltinEvaluation::Admitted && binding.is_some_and(|binding| !binding.builtin)
    {
        return Err(SQLError::Unsupported(
            "bound user function requires a logical engine session".into(),
        ));
    }
    if let (Some(binding), Some(hook)) = (binding, context.engine) {
        match evaluation {
            BuiltinEvaluation::Session => hook.require_builtin_execute(binding)?,
            BuiltinEvaluation::Admitted => {
                hook.require_builtin_execute_with_control(binding, control)?
            }
        }
    }
    let name = binding.map_or(name, |binding| binding.name.as_str());
    let metadata = binding.and_then(|binding| context.builtin_metadata(binding));
    let _interval_style = if evaluation == BuiltinEvaluation::Admitted {
        if metadata.map_or_else(
            || crate::semantics::sets::static_setness::builtin_call_returns_set(name, binding),
            super::super::BuiltinCallMetadata::returns_set,
        ) {
            return Err(crate::semantics::sets::placement::set_valued_call_error());
        }
        Some(super::super::interval_style::call_scope(
            name,
            &arguments,
            context.engine,
            control,
        )?)
    } else {
        None
    };
    if let Some((binding, dispatch)) =
        binding.and_then(|binding| binding.dispatch.map(|dispatch| (binding, dispatch)))
    {
        if evaluation == BuiltinEvaluation::Session {
            if let Some(result) =
                super::super::random::eval_dispatched_random_function(dispatch, &arguments, context)
            {
                return Ok(control.finish(result?, None)?);
            }
        }
        if arguments.iter().any(|(name, _)| name.is_some()) {
            return Err(SQLError::Internal(format!(
                "bound {} expression retained a named argument",
                dispatch.label(),
            )));
        }
        let evaluated = MovedArguments::new(arguments, control)?;
        return eval_dispatched(binding, dispatch, &evaluated.values, control, context);
    }
    let collation = FunctionBinding::selected_input_collation(binding);
    let value = match evaluation {
        BuiltinEvaluation::Session => {
            let arguments = arguments
                .into_uncontrolled()
                .expect("ordinary bound arguments");
            let value =
                super::eval_function_call_inner(name, arguments, context, false, collation)?;
            control.finish(value, None)?
        }
        BuiltinEvaluation::Admitted => {
            let normalized =
                super::super::call_arguments::normalized_function_name_with_control(name, control)?;
            let name = normalized.as_ref();
            if arguments.iter().any(|(name, _)| name.is_some()) {
                let Some(positional) = builtin_named_args(name, &arguments, control)? else {
                    return Err(super::super::diagnostics::unknown_function_error(
                        name, &arguments,
                    ));
                };
                eval_scalar(name, &positional, control, context, collation)?
            } else {
                let evaluated = MovedArguments::new(arguments, control)?;
                eval_scalar(name, &evaluated.values, control, context, collation)?
            }
        }
    };
    if matches!(*value, Value::Int(_) | Value::Float(_)) {
        if let Some(binding) = binding {
            if let Some(width) =
                super::super::BuiltinCallMetadata::numeric_result(metadata, binding, control)?
            {
                return width.coerce(&value, control);
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
        dispatch,
        arguments,
        control,
        context,
        binding.input_collation,
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
    collation: u32,
) -> Result<Produced<Value>> {
    if matches!(name, "nextval" | "currval" | "lastval" | "setval") {
        return super::super::scalar_dispatch::eval_sequence_function_with_control(
            name, arguments, context, control,
        );
    }
    if matches!(name, "coalesce" | "nullif" | "greatest" | "least") {
        return super::super::scalar_core::selection_with_context(
            name, arguments, control, context,
        );
    }
    super::super::scalar_dispatch::eval_generated_scalar_function_with_context(
        name, arguments, control, context, collation,
    )
}

#[cfg(test)]
mod tests;
