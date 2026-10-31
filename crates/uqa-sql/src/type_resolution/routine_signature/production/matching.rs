//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::{
    common::base_type,
    overload_resolution::{
        canonical_column_type_name_with_control as canonical,
        canonical_routine_type_name_with_control, canonical_type_is_preferred,
        routine_type_accepts_implicit_cast,
    },
};
use super::super::{
    MatchedRoutineSignature, RoutineCallDescriptor, RoutineDefaultType, RoutineParameterDescriptor,
    RoutinePolymorphicFamily, RoutineSignatureMatchError, RoutineVariadicMode, RoutineVariadicPlan,
};
use super::mapping::{effective_declared_type, structural_mapping};
use super::polymorphic::{
    actual_accepts_polymorphic_target, collect_polymorphic_actual, default_substitutions,
    resolve_target, Substitutions, Target,
};
use super::{
    array_type, copy_default, inline_type, polymorphic_type, signature_error, Components, Result,
    RoutineSignatureProductionError,
};
use crate::ColumnType;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

#[expect(
    clippy::too_many_lines,
    reason = "ordered candidate matching retains each production failure and default diagnostic at its original decision"
)]
pub(crate) fn match_routine_candidate_with_control(
    parameters: &[RoutineParameterDescriptor],
    call: RoutineCallDescriptor<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<MatchedRoutineSignature>>> {
    let Some(mapping) = structural_mapping(parameters, call, control)? else {
        return Ok(None);
    };
    let mut substitutions = Substitutions::default();
    let (mut simple_used, mut simple_range_used, mut compatible_used, mut compatible_range_used) =
        (false, false, false, false);
    for parameter in parameters {
        let Some(polymorphic) = polymorphic_type(&parameter.type_name, control)? else {
            continue;
        };
        match polymorphic.family() {
            RoutinePolymorphicFamily::Simple => {
                simple_used = true;
                simple_range_used |= polymorphic.is_range_family();
            }
            RoutinePolymorphicFamily::Compatible => {
                compatible_used = true;
                compatible_range_used |= polymorphic.is_range_family();
            }
        }
    }
    let mut inputs = ProductionVec::new(*control);
    let mut simple_element = None;
    let mut simple_array = None;
    let mut simple_range_subtype = None;
    let mut compatible_element = None;
    let mut compatible_range_seen = false;
    for (argument, parameter) in mapping.argument_positions.iter().copied().enumerate() {
        let declared = effective_declared_type(
            &parameters[parameter],
            Some(parameter) == mapping.variadic_index
                && mapping.variadic_mode == RoutineVariadicMode::Pack,
            control,
        )?;
        let Some(polymorphic) = polymorphic_type(&declared, control)? else {
            continue;
        };
        let mut fields = Components::new(*control);
        let actual = call.argument_types[argument]
            .as_ref()
            .map(|ty| fields.ty(ty).map(RoutineDefaultType::Concrete))
            .transpose()?;
        inputs.push_produced(fields.finish((polymorphic, actual))?)?;
        let Some(actual) = call.argument_types[argument].as_ref() else {
            continue;
        };
        if !collect_polymorphic_actual(
            polymorphic,
            actual,
            &mut simple_element,
            &mut simple_array,
            &mut simple_range_subtype,
            &mut compatible_element,
            &mut compatible_range_seen,
            control,
        )? {
            return Ok(None);
        }
    }
    let mut default_error = None;
    if mapping.defaulted_parameters.is_empty() {
        if simple_used {
            let Some(element) = simple_element else {
                return Err(indeterminate(RoutinePolymorphicFamily::Simple, control));
            };
            substitutions.simple_array = Some(match simple_array {
                Some(array) => array,
                None => array_type(&element, control)?,
            });
            substitutions.simple_element = Some(element);
            if simple_range_used {
                let Some(subtype) = simple_range_subtype else {
                    return Err(indeterminate(RoutinePolymorphicFamily::Simple, control));
                };
                substitutions.simple_range =
                    Some(inline_type(ColumnType::Range(subtype), control)?);
                substitutions.simple_multirange =
                    Some(inline_type(ColumnType::Multirange(subtype), control)?);
            }
        }
        if compatible_used {
            let element = match compatible_element {
                Some(element) => element,
                None => inline_type(ColumnType::Text, control)?,
            };
            substitutions.compatible_array = Some(array_type(&element, control)?);
            if compatible_range_used {
                if !compatible_range_seen {
                    return Err(indeterminate(RoutinePolymorphicFamily::Compatible, control));
                }
                let Some(subtype) = super::polymorphic::range_subtype_for_scalar(&element) else {
                    return Ok(None);
                };
                substitutions.compatible_range =
                    Some(inline_type(ColumnType::Range(subtype), control)?);
                substitutions.compatible_multirange =
                    Some(inline_type(ColumnType::Multirange(subtype), control)?);
            }
            substitutions.compatible_element = Some(element);
        }
    } else {
        for &index in mapping.defaulted_parameters.iter() {
            let parameter = &parameters[index];
            if let Some(polymorphic) = polymorphic_type(&parameter.type_name, control)? {
                let value = parameter
                    .default_type
                    .as_ref()
                    .map(|value| copy_default(value, control))
                    .transpose()?;
                let mut fields = Components::new(*control);
                let value = value.map(|value| fields.take(value));
                inputs.push_produced(fields.finish((polymorphic, value))?)?;
            }
        }
        match default_substitutions(&inputs, control) {
            Ok(resolved) => substitutions = resolved,
            Err(RoutineSignatureProductionError::Signature(error)) => default_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    drop(inputs);

    let mut signatures = ProductionVec::new(*control);
    let mut targets = ProductionVec::new(*control);
    let mut coercions = ProductionVec::new(*control);
    signatures.reserve(call.argument_types.len())?;
    targets.reserve(call.argument_types.len())?;
    coercions.reserve(call.argument_types.len())?;
    let (mut raw_exact_matches, mut exact_matches, mut preferred_matches) = (0, 0, 0);
    for (argument, parameter) in mapping.argument_positions.iter().copied().enumerate() {
        let expanded = Some(parameter) == mapping.variadic_index
            && mapping.variadic_mode == RoutineVariadicMode::Pack;
        let effective = effective_declared_type(&parameters[parameter], expanded, control)?;
        let actual = call.argument_types[argument].as_ref();
        let declared = parameters[parameter]
            .column_type
            .as_ref()
            .map(|ty| match ty {
                ColumnType::Array(element) if expanded => element.as_ref(),
                _ => ty,
            });
        let target = resolve_target(&effective, declared, actual, &substitutions, control)?;
        let Some(mut target) = (match target {
            Some(target) => Some(target),
            None => unresolved_target(&effective, default_error.is_some(), control)?,
        }) else {
            return Ok(None);
        };
        let polymorphic = polymorphic_type(&effective, control)?;
        if polymorphic.is_none() {
            if let Some(declared) = declared {
                target.ty = Some(declared.clone_with_control(control)?);
            }
        }
        if let Some(actual) = actual.filter(|_| *effective != "any") {
            if polymorphic.is_none() {
                let raw = canonical(actual, control)?;
                let base = canonical(base_type(actual), control)?;
                if *raw == *target.name {
                    raw_exact_matches += 1;
                    exact_matches += 1;
                } else if *base == *target.name {
                    exact_matches += 1;
                } else {
                    let declared_accepts = if let Some(ty) = target.ty.as_ref() {
                        let target_base = canonical(base_type(ty), control)?;
                        *base == *target_base
                            || routine_type_accepts_implicit_cast(&base, &target_base)
                    } else {
                        false
                    };
                    if declared_accepts || routine_type_accepts_implicit_cast(&base, &target.name) {
                        preferred_matches += usize::from(canonical_type_is_preferred(&target.name));
                    } else {
                        return Ok(None);
                    }
                }
            } else if default_error.is_none()
                && !actual_accepts_polymorphic_target(actual, &target, control)?
            {
                return Ok(None);
            }
        }
        signatures.push_produced(effective)?;
        targets.push_produced(control.copy_text(&target.name)?)?;
        coercions.push_produced(target.finish(control)?)?;
    }

    let mut identities = ProductionVec::new(*control);
    identities.reserve(parameters.len())?;
    for parameter in parameters {
        identities.push_produced(canonical_routine_type_name_with_control(
            &parameter.type_name,
            control,
        )?)?;
    }
    let mut parameter_types = ProductionVec::new(*control);
    let mut type_values = ProductionVec::new(*control);
    parameter_types.reserve(parameters.len())?;
    type_values.reserve(parameters.len())?;
    for (index, parameter) in parameters.iter().enumerate() {
        let actual = mapping.slots[index]
            .first()
            .and_then(|argument| call.argument_types[*argument].as_ref());
        let target = resolve_target(
            &parameter.type_name,
            parameter.column_type.as_ref(),
            actual,
            &substitutions,
            control,
        )?;
        let Some(target) = (match target {
            Some(target) => Some(target),
            None => unresolved_target(&parameter.type_name, default_error.is_some(), control)?,
        }) else {
            return Ok(None);
        };
        parameter_types.push_produced(target.name)?;
        let (ty, memory) = match target.ty {
            Some(ty) => {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        type_values.push_produced(control.finish(ty, memory)?)?;
    }
    let variadic_plan = {
        let mut fields = Components::new(*control);
        let value = match (mapping.variadic_index, mapping.variadic_mode) {
            (None, RoutineVariadicMode::None) => RoutineVariadicPlan::None,
            (Some(index), RoutineVariadicMode::Pack) => {
                let mut arguments = ProductionVec::new(*control);
                arguments.extend_from_slice(&mapping.slots[index])?;
                RoutineVariadicPlan::Pack {
                    parameter_index: index,
                    argument_indices: fields.take(arguments.finish()?),
                    element_type: fields.text(
                        &targets[*mapping.slots[index]
                            .first()
                            .expect("variadic pack argument")],
                    )?,
                    array_type: fields.text(&parameter_types[index])?,
                }
            }
            (Some(index), RoutineVariadicMode::PassThrough) => RoutineVariadicPlan::PassThrough {
                parameter_index: index,
                argument_index: *mapping.slots[index]
                    .first()
                    .expect("explicit variadic argument"),
                array_type: fields.text(&parameter_types[index])?,
            },
            (Some(index), RoutineVariadicMode::Default) => RoutineVariadicPlan::Default {
                parameter_index: index,
                array_type: fields.text(&parameter_types[index])?,
            },
            _ => unreachable!("structural variadic mode"),
        };
        fields.finish(value)?
    };
    let mut sources = ProductionVec::new(*control);
    sources.reserve(call.argument_types.len())?;
    for ty in call.argument_types {
        let (name, memory) = match ty {
            Some(ty) => {
                let (name, memory) = ty.catalog_name_with_control(control)?.into_parts();
                (Some(name), memory)
            }
            None => (None, control.empty_reservation()),
        };
        sources.push_produced(control.finish(name, memory)?)?;
    }
    let mut fields = Components::new(*control);
    let matched = MatchedRoutineSignature {
        declared_identity: fields.take(identities.finish()?),
        argument_signature: fields.take(signatures.finish()?),
        default_error: default_error.map(|error| fields.take(error)),
        argument_targets: fields.take(targets.finish()?),
        argument_sources: fields.take(sources.finish()?),
        argument_positions: fields.take(mapping.argument_positions.finish()?),
        coercion_targets: fields.take(coercions.finish()?),
        parameter_types: fields.take(parameter_types.finish()?),
        parameter_type_values: fields.take(type_values.finish()?),
        defaulted_parameters: fields.take(mapping.defaulted_parameters.finish()?),
        variadic_mode: mapping.variadic_mode,
        variadic_plan: fields.take(variadic_plan),
        substitutions: fields.take(substitutions.finish(control)?),
        raw_exact_matches,
        exact_matches,
        preferred_matches,
    };
    Ok(Some(fields.finish(matched)?))
}

fn unresolved_target(
    declared: &str,
    has_error: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Target>> {
    Ok(
        if has_error && polymorphic_type(declared, control)?.is_some() {
            Some(Target {
                name: canonical_routine_type_name_with_control(declared, control)?,
                ty: None,
            })
        } else {
            None
        },
    )
}

fn indeterminate(
    family: RoutinePolymorphicFamily,
    control: &ProductionControl<'_>,
) -> RoutineSignatureProductionError {
    signature_error(control, |_| {
        Ok(RoutineSignatureMatchError::IndeterminatePolymorphicType { family })
    })
}
