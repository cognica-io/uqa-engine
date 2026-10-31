//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned selected calls materialize argument and binding destinations before publication.

use super::{owned_arguments, RoutineInliningContext};
use crate::{
    ast::{
        CreateFunction, FunctionBinding, FunctionDispatch, RoutineInvocationBinding,
        RoutineVariadicMode,
    },
    plan::OwnedScalarExpr,
    type_resolution::FunctionBindingParts,
    SQLError, ScalarExpr,
};
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl, ProductionVec};

impl RoutineInliningContext<'_> {
    /// Normalize a selected call without separating its scalar children from their allocation owners. Declined materialization returns the complete original input.
    pub fn materialize_owned_call(
        &self,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        control.check()?;
        let ScalarExpr::Func {
            binding: Some(binding),
            args,
            ..
        } = &*expression
        else {
            return Ok(expression);
        };
        let Some((selected, arguments)) =
            self.materialized_owned_arguments(binding, args, control)?
        else {
            return Ok(expression);
        };
        let output = expression.replace_call_arguments(arguments, control)?;
        Ok(output.replace_call_binding(Some(selected), control)?)
    }

    pub(super) fn materialized_owned_arguments(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        control: &ProductionControl<'_>,
    ) -> Result<Option<(Produced<FunctionBinding>, OwnedScalarExpr)>, SQLError> {
        let Some(function) =
            super::super::selected_bound_function_with_control(self.routines, binding, control)?
        else {
            return Ok(None);
        };
        let Some(invocation) = binding.invocation.as_deref() else {
            return Ok(None);
        };
        super::super::invocation::validate_specialized_definition(&function.def, invocation)?;
        // Parameter modes and retained defaults do not change during polymorphic specialization; all coercion targets already come from the concrete invocation.
        let definition = &function.def;
        let Some(arguments) =
            owned_arguments::prepare(self, definition, invocation, arguments, control)?
        else {
            return Ok(None);
        };
        let selected = materialized_binding(binding, definition, invocation, control)?;
        let variadic =
            selected
                .invocation
                .as_ref()
                .and_then(|invocation| match invocation.variadic_mode {
                    RoutineVariadicMode::Explicit { parameter_index } => Some((
                        parameter_index,
                        invocation
                            .argument_positions
                            .iter()
                            .position(|index| *index == parameter_index),
                    )),
                    _ => None,
                });
        let mut arguments = arguments.into_container(control)?;
        if let Some((_, position)) = variadic {
            let position = position
                .ok_or_else(|| SQLError::Internal("variadic call lost its argument".into()))?;
            arguments = arguments.map_child(position, control, |argument| {
                let marker = FunctionBinding::dispatched_with_control(
                    FunctionDispatch::VariadicArgument,
                    control,
                )?;
                let name = control.copy_text(&marker.name)?;
                Ok::<_, SQLError>(OwnedScalarExpr::call(
                    name,
                    Some(marker),
                    std::iter::once(argument),
                    control,
                )?)
            })?;
        }
        Ok(Some((selected, arguments)))
    }
}

pub(super) fn materialized_binding(
    binding: &FunctionBinding,
    definition: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    control: &ProductionControl<'_>,
) -> Result<Produced<FunctionBinding>, SQLError> {
    FunctionBindingParts {
        object_id: binding.object_id,
        name: control.copy_text(&binding.name)?,
        argument_types: texts(&binding.argument_types, control)?,
        builtin: binding.builtin,
        dispatch: binding.dispatch,
        invocation: Some(materialized_invocation(definition, invocation, control)?),
        value_binding: binding
            .value_binding
            .as_deref()
            .map(|value| crate::plan::copy::copy_syntax_with_control(value, control))
            .transpose()?,
        resolution_error: binding
            .resolution_error
            .as_deref()
            .map(|error| crate::plan::copy::copy_syntax_with_control(error, control))
            .transpose()?,
        input_collation: binding.input_collation,
    }
    .finish(control)
}

fn materialized_invocation(
    definition: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoutineInvocationBinding>, SQLError> {
    let mut positions = ProductionVec::new(*control);
    positions.reserve(definition.identity_arity())?;
    for (index, parameter) in definition.params.iter().enumerate() {
        control.check()?;
        if super::super::body_parameters::is_sql_body_parameter(parameter) {
            positions.push_copy(index)?;
        }
    }
    let positions = positions.finish()?;
    let mut targets = ProductionVec::new(*control);
    targets.reserve(positions.len())?;
    for index in &*positions {
        targets.push_produced(control.copy_text(&invocation.parameter_types[*index])?)?;
    }
    let targets = targets.finish()?;
    let mut sources = ProductionVec::new(*control);
    sources.reserve(targets.len())?;
    for target in &*targets {
        let (target, memory) = control.copy_text(target)?.into_parts();
        sources.push_produced(control.finish(Some(target), memory)?)?;
    }
    let sources = sources.finish()?;
    let parameter_types = texts(&invocation.parameter_types, control)?;
    let return_type = invocation
        .return_type
        .as_deref()
        .map(|name| control.copy_text(name))
        .transpose()?;
    let mut memory = control.empty_reservation();
    let value = RoutineInvocationBinding {
        argument_positions: take(positions, &mut memory, control),
        argument_targets: take(targets, &mut memory, control),
        argument_sources: take(sources, &mut memory, control),
        parameter_types: take(parameter_types, &mut memory, control),
        return_type: return_type.map(|name| take(name, &mut memory, control)),
        variadic_mode: match invocation.variadic_mode {
            RoutineVariadicMode::Expanded { parameter_index } => {
                RoutineVariadicMode::Explicit { parameter_index }
            }
            mode => mode,
        },
    };
    Ok(control.finish(value, memory)?)
}

fn texts(
    values: &[String],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    output.reserve(values.len())?;
    for value in values {
        output.push_produced(control.copy_text(value)?)?;
    }
    Ok(output.finish()?)
}

fn take<T>(
    produced: Produced<T>,
    memory: &mut Option<MemoryReservation>,
    control: &ProductionControl<'_>,
) -> T {
    let (value, retained) = produced.into_parts();
    *memory = control.combine(memory.take(), retained);
    value
}
