//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One structural matcher borrows ordinary or fixed registry parameter declarations and owns its scratch/results.

use super::{
    canonical_column_type_name_with_control, canonical_routine_type_name_with_control,
    canonical_type_is_preferred, routine_type_accepts_implicit_cast, FunctionParameterDescriptor,
    MatchedFunctionSignature,
};
use crate::ast::ColumnType;
use crate::type_resolution::common::base_type;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ValueRetentionError,
};

pub(in crate::type_resolution) trait SignatureParameters {
    fn parameter_count(&self) -> usize;
    fn name(&self, index: usize) -> Option<&str>;
    fn has_default(&self, index: usize) -> bool;
    fn canonical_type(
        &self,
        index: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError>;
}

impl SignatureParameters for [FunctionParameterDescriptor] {
    fn parameter_count(&self) -> usize {
        self.len()
    }
    fn name(&self, index: usize) -> Option<&str> {
        self[index].name.as_deref()
    }
    fn has_default(&self, index: usize) -> bool {
        self[index].has_default
    }
    fn canonical_type(
        &self,
        index: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        canonical_routine_type_name_with_control(&self[index].type_name, control)
    }
}

/// Match a call against one declared signature, including named arguments, omitted defaults, domain exactness, implicit casts, and preferred types.
#[must_use]
pub fn match_function_signature(
    parameters: &[FunctionParameterDescriptor],
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> Option<MatchedFunctionSignature> {
    match_signature_with_control(
        parameters,
        argument_names,
        argument_types,
        &ProductionControl::uncontrolled(),
    )
    .expect("ordinary signature matching cannot be cancelled or limited")
    .map(|matched| {
        matched
            .into_uncontrolled()
            .expect("ordinary signature matching has no reservation")
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "one matching pass preserves named/default slot order and coercion scoring"
)]
pub(in crate::type_resolution) fn match_signature_with_control<
    P: SignatureParameters + ?Sized,
    N: AsRef<str>,
>(
    parameters: &P,
    argument_names: &[Option<N>],
    argument_types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<MatchedFunctionSignature>>, ValueRetentionError> {
    control.check()?;
    if argument_types.len() > parameters.parameter_count()
        || argument_names.len() != argument_types.len()
    {
        return Ok(None);
    }
    let mut slots = filled(None, parameters.parameter_count(), control)?;
    let mut argument_positions = filled(usize::MAX, argument_types.len(), control)?;
    let mut reserved = filled(false, parameters.parameter_count(), control)?;
    let mut saw_named = false;
    for (argument_index, argument_name) in argument_names.iter().enumerate() {
        control.check()?;
        if let Some(argument_name) = argument_name {
            saw_named = true;
            let Some(parameter_index) = (0..parameters.parameter_count())
                .find(|index| parameters.name(*index) == Some(argument_name.as_ref()))
            else {
                return Ok(None);
            };
            if reserved[parameter_index] {
                return Ok(None);
            }
            reserved.as_mut_slice()[parameter_index] = true;
            argument_positions.as_mut_slice()[argument_index] = parameter_index;
        } else if saw_named {
            return Ok(None);
        }
    }
    let positional_count = argument_names
        .iter()
        .take_while(|name| name.is_none())
        .count();
    let mut parameter_index = 0_usize;
    for (argument_index, argument_position) in argument_positions
        .as_mut_slice()
        .iter_mut()
        .take(positional_count)
        .enumerate()
    {
        control.check()?;
        let remaining_arguments = positional_count - argument_index;
        loop {
            control.check()?;
            if parameter_index >= parameters.parameter_count() || reserved[parameter_index] {
                return Ok(None);
            }
            let required_remaining = (parameter_index..parameters.parameter_count())
                .filter(|index| !reserved[*index] && !parameters.has_default(*index))
                .count();
            // PostgreSQL reserves required OUT/TABLE slots after defaulted inputs, so a positional placeholder binds the output slot when exactly the required arguments remain.
            if parameters.has_default(parameter_index) && remaining_arguments == required_remaining
            {
                parameter_index += 1;
                continue;
            }
            *argument_position = parameter_index;
            parameter_index += 1;
            break;
        }
    }
    for (argument_index, argument_type) in argument_types.iter().enumerate() {
        control.check()?;
        let index = argument_positions[argument_index];
        if index == usize::MAX {
            return Ok(None);
        }
        if slots.as_mut_slice()[index]
            .replace(argument_type.as_ref())
            .is_some()
        {
            return Ok(None);
        }
    }
    let mut matched_argument_types = ProductionVec::new(*control);
    matched_argument_types.reserve(argument_positions.len())?;
    for &index in argument_positions.iter() {
        matched_argument_types.push_produced(parameters.canonical_type(index, control)?)?;
    }
    let mut raw_exact_matches = 0_usize;
    let mut exact_matches = 0_usize;
    let mut preferred_matches = 0_usize;
    for (index, slot) in slots.iter().enumerate() {
        control.check()?;
        let Some(actual) = slot else {
            if !parameters.has_default(index) {
                return Ok(None);
            }
            continue;
        };
        let Some(actual_type) = actual else {
            continue;
        };
        let declared = parameters.canonical_type(index, control)?;
        let raw_actual = canonical_column_type_name_with_control(actual_type, control)?;
        let actual = canonical_column_type_name_with_control(base_type(actual_type), control)?;
        if *raw_actual == *declared {
            raw_exact_matches += 1;
            exact_matches += 1;
        } else if *actual == *declared {
            exact_matches += 1;
        } else if routine_type_accepts_implicit_cast(&actual, &declared) {
            preferred_matches += usize::from(canonical_type_is_preferred(&declared));
        } else {
            return Ok(None);
        }
    }
    let matched_argument_types = matched_argument_types.finish()?;
    let (argument_types, type_memory) = matched_argument_types.into_parts();
    let (argument_positions, position_memory) = argument_positions.into_parts();
    control
        .finish(
            MatchedFunctionSignature {
                argument_types,
                argument_positions,
                raw_exact_matches,
                exact_matches,
                preferred_matches,
            },
            control.combine(type_memory, position_memory),
        )
        .map(Some)
}

fn filled<T: Copy>(
    value: T,
    count: usize,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<T>>, ValueRetentionError> {
    let mut values = ProductionVec::new(*control);
    values.reserve(count)?;
    for _ in 0..count {
        values.push_copy(value)?;
    }
    values.finish()
}

#[cfg(test)]
mod tests;
