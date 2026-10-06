//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coercion planning and ranked result construction for one routine candidate.

use crate::ast::{ColumnType, RangeSubtype, RoutineDefaultType};

use super::super::common::base_type;
use super::super::overload_resolution::{
    canonical_column_type_name, canonical_routine_type_name, routine_type_accepts_implicit_cast,
    routine_type_is_preferred,
};
use super::mapping::{effective_declared_type, structural_mapping};
use super::polymorphic::{
    actual_accepts_polymorphic_target, collect_polymorphic_actual, default_substitutions,
    range_subtype_for_scalar, resolve_target,
};
use super::{
    routine_polymorphic_type, MatchedRoutineSignature, RoutineCallDescriptor,
    RoutineCoercionTarget, RoutineParameterDescriptor, RoutinePolymorphicFamily,
    RoutineSignatureMatchError, RoutineTypeSubstitutions, RoutineVariadicMode, RoutineVariadicPlan,
};

/// Match one selected signature and require its defaults to have consistent types.
pub fn match_routine_signature(
    parameters: &[RoutineParameterDescriptor],
    call: RoutineCallDescriptor<'_>,
) -> Result<Option<MatchedRoutineSignature>, RoutineSignatureMatchError> {
    let matched = match_routine_candidate(parameters, call)?;
    if let Some(error) = matched
        .as_ref()
        .and_then(|matched| matched.default_error.clone())
    {
        return Err(error);
    }
    Ok(matched)
}

#[expect(
    clippy::too_many_lines,
    reason = "type resolution preserves candidate order and ambiguity diagnostics atomically"
)]
pub(crate) fn match_routine_candidate(
    parameters: &[RoutineParameterDescriptor],
    call: RoutineCallDescriptor<'_>,
) -> Result<Option<MatchedRoutineSignature>, RoutineSignatureMatchError> {
    let Some(mapping) = structural_mapping(parameters, call)? else {
        return Ok(None);
    };

    let mut substitutions = RoutineTypeSubstitutions::default();
    let mut simple_used = false;
    let mut simple_range_used = false;
    let mut compatible_used = false;
    let mut compatible_range_used = false;
    for parameter in parameters {
        let Some(polymorphic) = routine_polymorphic_type(&parameter.type_name) else {
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

    let mut polymorphic_inputs = Vec::new();
    let mut simple_element: Option<ColumnType> = None;
    let mut simple_array: Option<ColumnType> = None;
    let mut simple_range_subtype: Option<RangeSubtype> = None;
    let mut compatible_element: Option<ColumnType> = None;
    let mut compatible_range_seen = false;
    for (argument_index, parameter_index) in mapping.argument_positions.iter().copied().enumerate()
    {
        let effective_declared = effective_declared_type(
            &parameters[parameter_index],
            Some(parameter_index) == mapping.variadic_index
                && mapping.variadic_mode == RoutineVariadicMode::Pack,
        );
        let Some(polymorphic) = routine_polymorphic_type(&effective_declared) else {
            continue;
        };
        polymorphic_inputs.push((
            polymorphic,
            call.argument_types[argument_index]
                .clone()
                .map(RoutineDefaultType::Concrete),
        ));
        let Some(actual) = call.argument_types[argument_index].as_ref() else {
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
        ) {
            return Ok(None);
        }
    }

    let mut default_error = None;
    if mapping.defaulted_parameters.is_empty() {
        if simple_used {
            let Some(element) = simple_element else {
                return Err(RoutineSignatureMatchError::IndeterminatePolymorphicType {
                    family: RoutinePolymorphicFamily::Simple,
                });
            };
            substitutions.simple_array =
                Some(simple_array.unwrap_or_else(|| ColumnType::Array(Box::new(element.clone()))));
            substitutions.simple_element = Some(element);
            if simple_range_used {
                let Some(subtype) = simple_range_subtype else {
                    return Err(RoutineSignatureMatchError::IndeterminatePolymorphicType {
                        family: RoutinePolymorphicFamily::Simple,
                    });
                };
                substitutions.simple_range = Some(ColumnType::Range(subtype));
                substitutions.simple_multirange = Some(ColumnType::Multirange(subtype));
            }
        }
        if compatible_used {
            let element = compatible_element.unwrap_or(ColumnType::Text);
            substitutions.compatible_array = Some(ColumnType::Array(Box::new(element.clone())));
            if compatible_range_used {
                if !compatible_range_seen {
                    return Err(RoutineSignatureMatchError::IndeterminatePolymorphicType {
                        family: RoutinePolymorphicFamily::Compatible,
                    });
                }
                let Some(subtype) = range_subtype_for_scalar(&element) else {
                    return Ok(None);
                };
                substitutions.compatible_range = Some(ColumnType::Range(subtype));
                substitutions.compatible_multirange = Some(ColumnType::Multirange(subtype));
            }
            substitutions.compatible_element = Some(element);
        }
    } else {
        for &index in &mapping.defaulted_parameters {
            let parameter = &parameters[index];
            if let Some(polymorphic) = routine_polymorphic_type(&parameter.type_name) {
                polymorphic_inputs.push((polymorphic, parameter.default_type.clone()));
            }
        }
        match default_substitutions(&polymorphic_inputs) {
            Ok(resolved) => substitutions = resolved,
            Err(error) => default_error = Some(error),
        }
    }

    let mut argument_signature = Vec::with_capacity(call.argument_types.len());
    let mut argument_targets = Vec::with_capacity(call.argument_types.len());
    let mut coercion_targets = Vec::with_capacity(call.argument_types.len());
    let mut raw_exact_matches = 0usize;
    let mut exact_matches = 0usize;
    let mut preferred_matches = 0usize;
    for (argument_index, parameter_index) in mapping.argument_positions.iter().copied().enumerate()
    {
        let effective_declared = effective_declared_type(
            &parameters[parameter_index],
            Some(parameter_index) == mapping.variadic_index
                && mapping.variadic_mode == RoutineVariadicMode::Pack,
        );
        argument_signature.push(effective_declared.clone());
        let actual = call.argument_types[argument_index].as_ref();
        let declared = parameters[parameter_index].column_type.as_ref().map(|ty| {
            if Some(parameter_index) == mapping.variadic_index
                && mapping.variadic_mode == RoutineVariadicMode::Pack
            {
                if let ColumnType::Array(element) = ty {
                    return element.as_ref().clone();
                }
            }
            ty.clone()
        });
        let Some(mut target) = resolve_target(
            &effective_declared,
            declared.as_ref(),
            actual,
            &substitutions,
        )
        .or_else(|| unresolved_candidate_target(&effective_declared, default_error.as_ref())) else {
            return Ok(None);
        };
        if routine_polymorphic_type(&effective_declared).is_none() {
            target.column_type = declared.or(target.column_type);
        }
        if let Some(actual) = actual {
            if routine_polymorphic_type(&effective_declared).is_none() {
                let raw_actual = canonical_column_type_name(actual);
                let base_actual = canonical_column_type_name(base_type(actual));
                if raw_actual == target.type_name {
                    raw_exact_matches += 1;
                    exact_matches += 1;
                } else if base_actual == target.type_name {
                    exact_matches += 1;
                } else if target.column_type.as_ref().is_some_and(|target| {
                    let target_base = canonical_column_type_name(base_type(target));
                    base_actual == target_base
                        || routine_type_accepts_implicit_cast(&base_actual, &target_base)
                }) || routine_type_accepts_implicit_cast(&base_actual, &target.type_name)
                {
                    preferred_matches += usize::from(routine_type_is_preferred(&target.type_name));
                } else {
                    return Ok(None);
                }
            } else if default_error.is_none() && !actual_accepts_polymorphic_target(actual, &target)
            {
                return Ok(None);
            }
        }
        argument_targets.push(target.type_name.clone());
        coercion_targets.push(target);
    }

    let declared_identity = parameters
        .iter()
        .map(|parameter| canonical_routine_type_name(&parameter.type_name))
        .collect::<Vec<_>>();
    let mut parameter_types = Vec::with_capacity(parameters.len());
    let mut parameter_type_values = Vec::with_capacity(parameters.len());
    for (parameter_index, parameter) in parameters.iter().enumerate() {
        let actual = mapping.slots[parameter_index]
            .first()
            .and_then(|argument_index| call.argument_types[*argument_index].as_ref());
        let Some(target) = resolve_target(
            &parameter.type_name,
            parameter.column_type.as_ref(),
            actual,
            &substitutions,
        )
        .or_else(|| unresolved_candidate_target(&parameter.type_name, default_error.as_ref())) else {
            return Ok(None);
        };
        parameter_types.push(target.type_name);
        parameter_type_values.push(target.column_type);
    }

    let variadic_plan = match (mapping.variadic_index, mapping.variadic_mode) {
        (None, RoutineVariadicMode::None) => RoutineVariadicPlan::None,
        (Some(parameter_index), RoutineVariadicMode::Pack) => RoutineVariadicPlan::Pack {
            parameter_index,
            argument_indices: mapping.slots[parameter_index].clone(),
            element_type: mapping.slots[parameter_index]
                .first()
                .map(|argument_index| argument_targets[*argument_index].clone())
                .expect("a variadic pack has at least one argument"),
            array_type: parameter_types[parameter_index].clone(),
        },
        (Some(parameter_index), RoutineVariadicMode::PassThrough) => {
            RoutineVariadicPlan::PassThrough {
                parameter_index,
                argument_index: *mapping.slots[parameter_index]
                    .first()
                    .expect("an explicit variadic call has one array argument"),
                array_type: parameter_types[parameter_index].clone(),
            }
        }
        (Some(parameter_index), RoutineVariadicMode::Default) => RoutineVariadicPlan::Default {
            parameter_index,
            array_type: parameter_types[parameter_index].clone(),
        },
        _ => unreachable!("structural mapping returned an inconsistent variadic mode"),
    };

    Ok(Some(MatchedRoutineSignature {
        declared_identity,
        argument_signature,
        default_error,
        argument_targets,
        // Invocation re-resolves the source types, so user-defined types are recorded by OID identity rather than by a name the search path may not reach.
        argument_sources: call
            .argument_types
            .iter()
            .map(|ty| ty.as_ref().map(ColumnType::catalog_name))
            .collect(),
        argument_positions: mapping.argument_positions,
        coercion_targets,
        parameter_types,
        parameter_type_values,
        defaulted_parameters: mapping.defaulted_parameters,
        variadic_mode: mapping.variadic_mode,
        variadic_plan,
        substitutions,
        raw_exact_matches,
        exact_matches,
        preferred_matches,
    }))
}

/// A failed candidate carries only its declared identity, never a fabricated value
/// type. Its error must be raised before this candidate can publish a binding.
fn unresolved_candidate_target(
    declared: &str,
    error: Option<&RoutineSignatureMatchError>,
) -> Option<RoutineCoercionTarget> {
    (error.is_some() && routine_polymorphic_type(declared).is_some()).then(|| {
        RoutineCoercionTarget {
            type_name: canonical_routine_type_name(declared),
            column_type: None,
        }
    })
}
