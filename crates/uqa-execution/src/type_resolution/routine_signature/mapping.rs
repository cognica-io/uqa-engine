//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named, positional, defaulted, and variadic call mapping.

use super::{
    canonical_routine_type_name, RoutineCallDescriptor, RoutineParameterDescriptor,
    RoutineSignatureMatchError, RoutineVariadicMode,
};

#[derive(Debug)]
pub(super) struct StructuralMapping {
    pub(super) argument_positions: Vec<usize>,
    pub(super) slots: Vec<Vec<usize>>,
    pub(super) defaulted_parameters: Vec<usize>,
    pub(super) variadic_index: Option<usize>,
    pub(super) variadic_mode: RoutineVariadicMode,
}

pub(super) fn structural_mapping(
    parameters: &[RoutineParameterDescriptor],
    call: RoutineCallDescriptor<'_>,
) -> Result<Option<StructuralMapping>, RoutineSignatureMatchError> {
    if call.argument_names.len() != call.argument_types.len() {
        return Ok(None);
    }
    let variadic_indices = parameters
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| parameter.variadic.then_some(index))
        .collect::<Vec<_>>();
    if variadic_indices.len() > 1
        || variadic_indices
            .first()
            .is_some_and(|index| *index + 1 != parameters.len())
    {
        return Err(RoutineSignatureMatchError::InvalidVariadicSignature {
            reason: "VARIADIC parameter must be the final and only variadic parameter".into(),
        });
    }
    let variadic_index = variadic_indices.first().copied();
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
        if !is_array_declaration(&parameters[index].type_name) {
            return Err(RoutineSignatureMatchError::InvalidVariadicSignature {
                reason: "VARIADIC parameter must have an array type".into(),
            });
        }
        if !parameters[index].has_default
            && parameters[..index]
                .iter()
                .any(|parameter| parameter.has_default)
        {
            return Err(RoutineSignatureMatchError::InvalidVariadicSignature {
                reason: "input parameters after one with a default value must also have defaults"
                    .into(),
            });
        }
    } else if call.argument_types.len() > parameters.len() {
        return Ok(None);
    }

    let mut argument_positions = vec![usize::MAX; call.argument_types.len()];
    let mut reserved = vec![false; parameters.len()];
    let mut saw_named = false;
    for (argument_index, argument_name) in call.argument_names.iter().enumerate() {
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
            reserved[parameter_index] = true;
            argument_positions[argument_index] = parameter_index;
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
        .iter_mut()
        .take(positional_count)
        .enumerate()
    {
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
    let mut slots = vec![Vec::new(); parameters.len()];
    for (argument_index, parameter_index) in argument_positions.iter().copied().enumerate() {
        if !slots[parameter_index].is_empty()
            && (Some(parameter_index) != variadic_index || call.explicit_variadic)
        {
            return Ok(None);
        }
        slots[parameter_index].push(argument_index);
    }

    let mut defaulted_parameters = Vec::new();
    for (index, (parameter, slot)) in parameters.iter().zip(&slots).enumerate() {
        if slot.is_empty() {
            if !parameter.has_default {
                return Ok(None);
            }
            defaulted_parameters.push(index);
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

fn is_array_declaration(type_name: &str) -> bool {
    let canonical = canonical_routine_type_name(type_name);
    canonical.ends_with("[]")
        || matches!(
            canonical.as_str(),
            "anyarray" | "anycompatiblearray" | "int2vector" | "oidvector"
        )
}

pub(super) fn effective_declared_type(
    parameter: &RoutineParameterDescriptor,
    expanded: bool,
) -> String {
    let canonical = canonical_routine_type_name(&parameter.type_name);
    if !expanded {
        return canonical;
    }
    match canonical.as_str() {
        "anyarray" => "anyelement".into(),
        "anycompatiblearray" => "anycompatible".into(),
        "int2vector" => "int2".into(),
        "oidvector" => "oid".into(),
        _ => canonical
            .strip_suffix("[]")
            .expect("validated variadic array declaration")
            .into(),
    }
}
