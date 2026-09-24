//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed built-in selection borrows static declarations and shares signature matching and ranking with catalog overloads.

use super::{registry, ResolvedFunctionOverload};
use crate::type_resolution::overload_resolution::{
    bound_function_resolution_error, candidates::Candidates, function_resolution_error,
    match_signature_with_control, rank_function_matches_with_control, MatchedFunctionSignature,
    RankedFunctionMatch,
};
use crate::{ast::FunctionBinding, ColumnType, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(super) fn resolve_overload_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<ResolvedFunctionOverload>, SQLError> {
    let selected = select_with_control(
        name,
        binding,
        argument_names,
        argument_types,
        explicit_variadic,
        control,
    )?;
    let name = control.format(format_args!("pg_catalog.{}", selected.name))?;
    let return_type = selected
        .declaration
        .return_type
        .clone_with_control(control)?;
    let mut types = ProductionVec::new(*control);
    types.reserve(selected.declaration.argument_types.len())?;
    for ty in selected.declaration.argument_types {
        types.push_produced(ty.sql_name_with_control(control)?)?;
    }
    let types = types.finish()?;
    let (name, name_memory) = name.into_parts();
    let (return_type, return_memory) = return_type.into_parts();
    let (types, types_memory) = types.into_parts();
    // No fallible work separates the transferred payloads from their next value-first owner.
    control
        .finish(
            ResolvedFunctionOverload {
                binding: FunctionBinding {
                    object_id: None,
                    name,
                    argument_types: types,
                    builtin: true,
                    dispatch: None,
                    invocation: None,
                    resolution_error: None,
                },
                return_type,
                exact_matches: selected.matched.exact_matches,
                known_arguments: argument_types.iter().flatten().count(),
                preferred_matches: selected.matched.preferred_matches,
                precedes_pg_catalog: false,
            },
            control.combine(name_memory, control.combine(return_memory, types_memory)),
        )
        .map_err(Into::into)
}

pub(super) struct SelectedSignature {
    pub(super) name: &'static str,
    pub(super) declaration: &'static registry::Signature,
    matched: Produced<MatchedFunctionSignature>,
}

impl RankedFunctionMatch for SelectedSignature {
    fn argument_types(&self) -> &[String] {
        &self.matched.argument_types
    }
    fn raw_exact_matches(&self) -> usize {
        self.matched.raw_exact_matches
    }
    fn exact_matches(&self) -> usize {
        self.matched.exact_matches
    }
    fn preferred_matches(&self) -> usize {
        self.matched.preferred_matches
    }
}

pub(super) fn select_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    control: &ProductionControl<'_>,
) -> Result<SelectedSignature, SQLError> {
    control.check()?;
    crate::expr::validate_named_argument_order_with_control(
        argument_names.iter().map(Option::as_deref),
        control,
    )?;
    let undefined = || {
        function_resolution_error(
            "42883",
            name,
            argument_names,
            argument_types,
            "does not exist",
        )
    };
    let (registered_name, declarations) = registry::lookup(name).ok_or_else(undefined)?;
    if explicit_variadic && argument_names.iter().any(Option::is_some) {
        return Err(undefined());
    }
    if let Some(binding) = binding {
        if registry::lookup(&binding.name).is_none_or(|(name, _)| name != registered_name) {
            return Err(bound_function_resolution_error(binding));
        }
        let declaration = registry::bound_signature(binding, control)?
            .ok_or_else(|| bound_function_resolution_error(binding))?;
        let matched =
            match_signature_with_control(declaration, argument_names, argument_types, control)?
                .ok_or_else(|| bound_function_resolution_error(binding))?;
        return Ok(SelectedSignature {
            name: registered_name,
            declaration,
            matched,
        });
    }
    let mut candidates = Candidates::new(control);
    for declaration in declarations {
        control.check()?;
        if let Some(matched) =
            match_signature_with_control(declaration, argument_names, argument_types, control)?
        {
            candidates.push(SelectedSignature {
                name: registered_name,
                declaration,
                matched,
            })?;
        }
    }
    let mut candidates = candidates.finish();
    if candidates.values.is_empty() {
        return Err(undefined());
    }
    if !rank_function_matches_with_control(&mut candidates.values, argument_types, control)?
        || candidates.values.len() != 1
    {
        return Err(function_resolution_error(
            "42725",
            name,
            argument_names,
            argument_types,
            "is not unique",
        ));
    }
    candidates
        .values
        .pop()
        .ok_or_else(|| SQLError::Internal("resolved fixed built-in candidate disappeared".into()))
}

#[cfg(test)]
mod tests;
