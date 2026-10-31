//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation remapping moves surviving argument metadata and admits only replacement parameter slots.

use crate::{
    ast::{
        CreateFunction, FunctionParamMode, FunctionReturns, RoutineInvocationBinding,
        RoutineVariadicMode,
    },
    type_resolution::{routine_polymorphic_type_with_control, MatchedRoutineSignature},
    ColumnType, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(super) fn binding(
    definition: &CreateFunction,
    parameter_indices: &[usize],
    matched: &MatchedRoutineSignature,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoutineInvocationBinding>, SQLError> {
    let mut concrete: crate::type_resolution::RoutineInvocationParts =
        matched.invocation_binding_parts_with_control(None, control)?;
    let mut parameter_types = ProductionVec::new(*control);
    parameter_types.reserve(definition.params.len())?;
    let mut output_count = 0;
    let mut first_output = None;
    for (definition_index, parameter) in definition.params.iter().enumerate() {
        control.check()?;
        let mut call_index = None;
        for (position, index) in parameter_indices.iter().enumerate() {
            control.check()?;
            if *index == definition_index {
                call_index = Some(position);
                break;
            }
        }
        let ty = match call_index.and_then(|index| concrete.parameter_types.get(index)) {
            Some(ty) => control.copy_text(ty)?,
            None => match output_type(matched, &parameter.type_name, control)? {
                Some(ty) => ty,
                None => control.copy_text(&parameter.type_name)?,
            },
        };
        parameter_types.push_produced(ty)?;
        if matches!(
            parameter.mode,
            FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
        ) {
            first_output.get_or_insert(definition_index);
            output_count += 1;
        }
    }
    let parameter_types = parameter_types.finish()?;
    let mut positions = ProductionVec::new(*control);
    positions.reserve(matched.argument_positions.len())?;
    for call_index in &matched.argument_positions {
        control.check()?;
        positions.push_copy(parameter_indices[*call_index])?;
    }
    let return_type = match &definition.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            output_type(matched, type_name, control)?
        }
        FunctionReturns::Table => Some(ColumnType::Record.catalog_name_with_control(control)?),
        FunctionReturns::None => match output_count {
            0 => None,
            1 => parameter_types
                .get(first_output.expect("one output parameter"))
                .map(|ty| control.copy_text(ty))
                .transpose()?,
            _ => Some(ColumnType::Record.catalog_name_with_control(control)?),
        },
    };
    concrete.argument_positions = positions.finish()?;
    concrete.parameter_types = parameter_types;
    concrete.return_type = return_type;
    concrete.variadic_mode = match concrete.variadic_mode {
        RoutineVariadicMode::Expanded { parameter_index } => RoutineVariadicMode::Expanded {
            parameter_index: parameter_indices[parameter_index],
        },
        RoutineVariadicMode::Explicit { parameter_index } => RoutineVariadicMode::Explicit {
            parameter_index: parameter_indices[parameter_index],
        },
        RoutineVariadicMode::None => RoutineVariadicMode::None,
    };
    concrete.finish(control)
}

fn output_type(
    matched: &MatchedRoutineSignature,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<String>>, SQLError> {
    if routine_polymorphic_type_with_control(name, control)?.is_some() {
        return Ok(matched
            .substitute_type_with_control(name, control)?
            .map(|ty| ty.catalog_name_with_control(control))
            .transpose()?);
    }
    Ok(Some(control.copy_text(name)?))
}
