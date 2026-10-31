//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named, positional, defaulted, and variadic call mapping.

use super::super::super::overload_resolution::canonical_routine_type_name_with_control;
use super::super::{
    RoutineCallDescriptor, RoutineParameterDescriptor, RoutineSignatureMatchError,
    RoutineVariadicMode,
};
use super::{signature_error, Result};
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl, ProductionVec};

pub(super) struct StructuralMapping<'a> {
    pub(super) argument_positions: ProductionVec<'a, usize>,
    pub(super) slots: Slots<'a>,
    pub(super) defaulted_parameters: ProductionVec<'a, usize>,
    pub(super) variadic_index: Option<usize>,
    pub(super) variadic_mode: RoutineVariadicMode,
}

pub(super) struct Slots<'a> {
    values: Vec<ProductionVec<'a, usize>>,
    _memory: Option<MemoryReservation>,
}

impl<'a> std::ops::Deref for Slots<'a> {
    type Target = [ProductionVec<'a, usize>];
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl std::ops::DerefMut for Slots<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.values
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "type resolution preserves candidate order and ambiguity diagnostics atomically"
)]
pub(super) fn structural_mapping<'a>(
    parameters: &[RoutineParameterDescriptor],
    call: RoutineCallDescriptor<'_>,
    control: &ProductionControl<'a>,
) -> Result<Option<StructuralMapping<'a>>> {
    control.check()?;
    if call.argument_names.len() != call.argument_types.len() {
        return Ok(None);
    }
    let mut variadic_indices = parameters
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| parameter.variadic.then_some(index));
    let variadic_index = variadic_indices.next();
    if variadic_indices.next().is_some()
        || variadic_index.is_some_and(|index| index + 1 != parameters.len())
    {
        return Err(signature_error(control, |fields| {
            Ok(RoutineSignatureMatchError::InvalidVariadicSignature {
                reason: fields
                    .text("VARIADIC parameter must be the final and only variadic parameter")?,
            })
        }));
    }
    if variadic_index.is_none()
        && call.explicit_variadic
        && call.argument_names.iter().any(Option::is_some)
    {
        return Ok(None);
    }
    // PostgreSQL excludes an expanded variadic candidate whenever named notation is used, even when every omitted variadic position has a default. An explicit `VARIADIC name => array` call still targets the declared array slot, and the marker does not exclude an otherwise matching fixed-array candidate.
    if variadic_index.is_some()
        && !call.explicit_variadic
        && call.argument_names.iter().any(Option::is_some)
    {
        return Ok(None);
    }
    if let Some(index) = variadic_index {
        if !is_array_declaration(&parameters[index].type_name, control)? {
            return Err(signature_error(control, |fields| {
                Ok(RoutineSignatureMatchError::InvalidVariadicSignature {
                    reason: fields.text("VARIADIC parameter must have an array type")?,
                })
            }));
        }
        if !parameters[index].has_default
            && parameters[..index]
                .iter()
                .any(|parameter| parameter.has_default)
        {
            return Err(signature_error(control, |fields| {
                Ok(RoutineSignatureMatchError::InvalidVariadicSignature {
                    reason: fields.text(
                        "input parameters after one with a default value must also have defaults",
                    )?,
                })
            }));
        }
    } else if call.argument_types.len() > parameters.len() {
        return Ok(None);
    }

    let mut argument_positions = buffer(usize::MAX, call.argument_types.len(), control)?;
    let mut reserved = buffer(false, parameters.len(), control)?;
    let mut saw_named = false;
    for (argument_index, argument_name) in call.argument_names.iter().enumerate() {
        control.check()?;
        if let Some(argument_name) = argument_name {
            saw_named = true;
            let Some(parameter_index) = parameters
                .iter()
                .position(|parameter| parameter.name.as_deref() == Some(argument_name.as_str()))
            else {
                return Ok(None);
            };
            if reserved[parameter_index]
                || Some(parameter_index) == variadic_index && !call.explicit_variadic
            {
                return Ok(None);
            }
            reserved.as_mut_slice()[parameter_index] = true;
            argument_positions.as_mut_slice()[argument_index] = parameter_index;
        } else if saw_named {
            return Ok(None);
        }
    }

    let positional_count = call
        .argument_names
        .iter()
        .take_while(|name| name.is_none())
        .count();
    let mut parameter_index = 0usize;
    for (argument_index, argument_position) in argument_positions
        .as_mut_slice()
        .iter_mut()
        .take(positional_count)
        .enumerate()
    {
        control.check()?;
        let remaining_arguments = positional_count - argument_index;
        loop {
            if parameter_index >= parameters.len() {
                if let Some(variadic_index) = variadic_index.filter(|_| !call.explicit_variadic) {
                    *argument_position = variadic_index;
                    break;
                }
                return Ok(None);
            }
            if reserved[parameter_index] {
                return Ok(None);
            }
            if Some(parameter_index) == variadic_index {
                if call.explicit_variadic {
                    if remaining_arguments != 1 {
                        return Ok(None);
                    }
                    *argument_position = parameter_index;
                    parameter_index += 1;
                } else {
                    *argument_position = parameter_index;
                }
                break;
            }
            let required_remaining = parameters[parameter_index..]
                .iter()
                .enumerate()
                .filter(|(offset, parameter)| {
                    let index = parameter_index + offset;
                    !reserved[index] && !parameter.has_default
                })
                .count();
            if parameters[parameter_index].has_default && remaining_arguments == required_remaining
            {
                parameter_index += 1;
                continue;
            }
            *argument_position = parameter_index;
            parameter_index += 1;
            break;
        }
    }

    if argument_positions.contains(&usize::MAX) {
        return Ok(None);
    }
    let mut slots = ProductionVec::new(*control);
    slots.reserve(parameters.len())?;
    for _ in parameters {
        slots.push_produced(
            control.finish(ProductionVec::new(*control), control.empty_reservation())?,
        )?;
    }
    let (values, memory) = slots.finish()?.into_parts();
    let mut slots = Slots {
        values,
        _memory: memory,
    };
    for (argument_index, parameter_index) in argument_positions.iter().copied().enumerate() {
        if !slots[parameter_index].is_empty()
            && (Some(parameter_index) != variadic_index || call.explicit_variadic)
        {
            return Ok(None);
        }
        slots[parameter_index].push_copy(argument_index)?;
    }

    let mut defaulted_parameters = ProductionVec::new(*control);
    for (index, (parameter, slot)) in parameters.iter().zip(slots.iter()).enumerate() {
        if slot.is_empty() {
            if !parameter.has_default {
                return Ok(None);
            }
            defaulted_parameters.push_copy(index)?;
        }
    }
    let variadic_mode = match variadic_index {
        None => RoutineVariadicMode::None,
        Some(index) if call.explicit_variadic => {
            if slots[index].len() != 1 {
                return Ok(None);
            }
            RoutineVariadicMode::PassThrough
        }
        Some(index) if slots[index].is_empty() => RoutineVariadicMode::Default,
        Some(_) => RoutineVariadicMode::Pack,
    };
    Ok(Some(StructuralMapping {
        argument_positions,
        slots,
        defaulted_parameters,
        variadic_index,
        variadic_mode,
    }))
}

fn is_array_declaration(type_name: &str, control: &ProductionControl<'_>) -> Result<bool> {
    let canonical = canonical_routine_type_name_with_control(type_name, control)?;
    Ok(canonical.ends_with("[]")
        || matches!(
            canonical.as_str(),
            "anyarray" | "anycompatiblearray" | "int2vector" | "oidvector"
        ))
}

pub(super) fn effective_declared_type(
    parameter: &RoutineParameterDescriptor,
    expanded: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let canonical = canonical_routine_type_name_with_control(&parameter.type_name, control)?;
    if !expanded {
        return Ok(canonical);
    }
    let element = match canonical.as_str() {
        "anyarray" => "anyelement",
        "anycompatiblearray" => "anycompatible",
        "int2vector" => "int2",
        "oidvector" => "oid",
        _ => canonical
            .strip_suffix("[]")
            .expect("validated variadic array declaration"),
    };
    Ok(control.copy_text(element)?)
}

fn buffer<'a, T: Copy>(
    value: T,
    count: usize,
    control: &ProductionControl<'a>,
) -> Result<ProductionVec<'a, T>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(count)?;
    for _ in 0..count {
        output.push_copy(value)?;
    }
    Ok(output)
}
