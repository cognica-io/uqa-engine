//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime evaluation of validated SQL call arguments.

use super::evaluator::eval_scalar_inner;
use super::{ScalarEvalContext, ScalarExpr};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::SQLError;

pub use uqa_sql::ir::{
    scalar_call_argument, scalar_call_arguments, validate_scalar_call_arguments, ScalarCallArgument,
};

type EvaluatedArguments = Vec<(Option<String>, Value)>;

pub fn eval_call_arguments(
    arguments: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
) -> Result<Vec<(Option<String>, Value)>, SQLError> {
    eval_call_arguments_with_control(arguments, context, &ProductionControl::uncontrolled()).map(
        |arguments| {
            arguments
                .into_uncontrolled()
                .expect("ordinary evaluated call arguments")
        },
    )
}

pub(super) fn eval_call_arguments_with_control(
    arguments: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<EvaluatedArguments>, SQLError> {
    let decoded = uqa_sql::ir::scalar_call_arguments_with_control(arguments, control)?;
    let mut output = ProductionVec::new(*control);
    output.reserve(decoded.len())?;
    for argument in &*decoded {
        let name = argument
            .name
            .map(|name| control.copy_text(name))
            .transpose()?;
        let value = eval_scalar_inner(argument.value, context, control)?;
        let (value, value_memory) = value.into_parts();
        let (name, name_memory) = name.map_or_else(
            || (None, control.empty_reservation()),
            |name| {
                let (name, memory) = name.into_parts();
                (Some(name), memory)
            },
        );
        output.push_produced(
            control.finish((name, value), control.combine(name_memory, value_memory))?,
        )?;
    }
    output.finish().map_err(Into::into)
}

/// Expand an explicit `VARIADIC` array passed to a `VARIADIC "any"` built-in into separately admitted values. `None` means a NULL array, for which the function returns NULL.
pub(super) fn expand_variadic_any_arguments(
    name: &str,
    arguments: &[ScalarExpr],
    evaluated: Produced<EvaluatedArguments>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<EvaluatedArguments>>, SQLError> {
    let explicit_variadic = arguments
        .last()
        .and_then(|argument| uqa_sql::scalar_call_argument(argument).ok())
        .is_some_and(|argument| argument.explicit_variadic);
    if !explicit_variadic || !uqa_sql::expr::variadic_any::is_variadic_any(name) {
        return Ok(Some(evaluated));
    }
    let (values, _) = evaluated.into_parts();
    let expanded = match uqa_sql::expr::variadic_any::expand_variadic_any(name, values)? {
        uqa_sql::expr::variadic_any::VariadicAnyArguments::Arguments(values) => values,
        uqa_sql::expr::variadic_any::VariadicAnyArguments::NullResult => return Ok(None),
    };
    let mut output = ProductionVec::new(*control);
    output.reserve(expanded.len())?;
    for (name, value) in expanded {
        let name = name.map(|name| control.copy_text(&name)).transpose()?;
        let (name, name_memory) = name.map_or_else(
            || (None, control.empty_reservation()),
            |name| {
                let (name, memory) = name.into_parts();
                (Some(name), memory)
            },
        );
        let (value, value_memory) = control.copy_value(&value)?.into_parts();
        output.push_produced(
            control.finish((name, value), control.combine(name_memory, value_memory))?,
        )?;
    }
    Ok(Some(output.finish()?))
}
