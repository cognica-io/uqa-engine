//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Local overload resolution borrows immutable declarations and owns matching workspace plus the selected binding.

use super::{
    bound_function_resolution_error, candidates::Candidates,
    canonical_routine_type_name_with_control, function_resolution_error,
    match_signature_with_control, rank_function_matches_with_control, MatchedBuiltinFunction,
    MatchedFunctionSignature, RankedFunctionMatch, SignatureParameters,
};
use crate::{
    ast::FunctionBinding,
    type_resolution::{BuiltinFunctionOverload, ResolvedFunctionOverload},
    ColumnType, SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ValueRetentionError,
};

impl SignatureParameters for BuiltinFunctionOverload {
    fn parameter_count(&self) -> usize {
        self.argument_types.len()
    }
    fn name(&self, index: usize) -> Option<&str> {
        self.argument_names[index].as_deref()
    }
    fn has_default(&self, index: usize) -> bool {
        index >= self.argument_types.len() - self.default_arguments
    }
    fn canonical_type(
        &self,
        index: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        let name = self.argument_types[index].sql_name_with_control(control)?;
        canonical_routine_type_name_with_control(&name, control)
    }
}

#[must_use]
pub fn builtin_name_matches(name: &str, builtin_name: &str) -> bool {
    if name.contains('.') {
        name.eq_ignore_ascii_case(builtin_name)
    } else {
        builtin_name
            .rsplit('.')
            .next()
            .is_some_and(|local| local.eq_ignore_ascii_case(name))
    }
}

#[must_use]
pub fn builtin_binding_matches(
    builtin: &BuiltinFunctionOverload,
    binding: &FunctionBinding,
) -> bool {
    binding_matches(builtin, binding, &ProductionControl::uncontrolled())
        .expect("ordinary binding matching cannot be cancelled or limited")
}

fn binding_matches(
    builtin: &BuiltinFunctionOverload,
    binding: &FunctionBinding,
    control: &ProductionControl<'_>,
) -> Result<bool, ValueRetentionError> {
    control.check()?;
    if !builtin.name.eq_ignore_ascii_case(&binding.name)
        || builtin.argument_types.len() != binding.argument_types.len()
    {
        return Ok(false);
    }
    for (ty, selected) in builtin.argument_types.iter().zip(&binding.argument_types) {
        let ty = ty.sql_name_with_control(control)?;
        let canonical = canonical_routine_type_name_with_control(&ty, control)?;
        let selected = canonical_routine_type_name_with_control(selected, control)?;
        if *canonical != *selected {
            return Ok(false);
        }
    }
    Ok(true)
}

fn match_builtin(
    builtin: &BuiltinFunctionOverload,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<MatchedFunctionSignature>>, ValueRetentionError> {
    if builtin.default_arguments > builtin.argument_types.len()
        || builtin.argument_names.len() != builtin.argument_types.len()
    {
        return Ok(None);
    }
    match_signature_with_control(builtin, argument_names, argument_types, control)
}

#[must_use]
pub fn match_builtin_function_overload(
    builtin: BuiltinFunctionOverload,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> Option<MatchedBuiltinFunction> {
    let matched = match_builtin(
        &builtin,
        argument_names,
        argument_types,
        &ProductionControl::uncontrolled(),
    )
    .expect("ordinary signature matching cannot be cancelled or limited")?
    .into_uncontrolled()
    .expect("ordinary matching workspace has no reservation");
    Some(MatchedBuiltinFunction {
        overload: builtin,
        argument_types: matched.argument_types,
        argument_positions: matched.argument_positions,
        raw_exact_matches: matched.raw_exact_matches,
        exact_matches: matched.exact_matches,
        preferred_matches: matched.preferred_matches,
    })
}

pub fn resolve_local_builtin_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtins: &[BuiltinFunctionOverload],
) -> Result<ResolvedFunctionOverload, SQLError> {
    super::resolve_local_builtin_overload_with_control(
        name,
        binding,
        argument_names,
        argument_types,
        builtins,
        &ProductionControl::uncontrolled(),
    )
    .map(|result| {
        result
            .into_uncontrolled()
            .expect("ordinary selected binding has no reservation")
    })
}

pub(in crate::type_resolution) fn resolve_local_builtin_overload_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtins: &[BuiltinFunctionOverload],
    control: &ProductionControl<'_>,
) -> Result<Produced<ResolvedFunctionOverload>, SQLError> {
    let selected = select_local_builtin_with_control(
        name,
        binding,
        argument_names,
        argument_types,
        builtins,
        control,
    )?;
    resolved(selected, argument_types, control)
}

pub(in crate::type_resolution) fn select_local_builtin_with_control<'a>(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtins: &'a [BuiltinFunctionOverload],
    control: &ProductionControl<'_>,
) -> Result<BorrowedMatch<'a>, SQLError> {
    control.check()?;
    if let Some(binding) = binding {
        if !binding.builtin {
            return Err(bound_function_resolution_error(binding));
        }
        for builtin in builtins {
            if binding_matches(builtin, binding, control)? {
                let matched = match_builtin(builtin, argument_names, argument_types, control)?
                    .ok_or_else(|| bound_function_resolution_error(binding))?;
                return Ok(BorrowedMatch {
                    builtin,
                    signature: matched,
                });
            }
        }
        return Err(bound_function_resolution_error(binding));
    }
    let mut candidates = Candidates::new(control);
    for builtin in builtins {
        control.check()?;
        if builtin_name_matches(name, &builtin.name) {
            if let Some(signature) =
                match_builtin(builtin, argument_names, argument_types, control)?
            {
                candidates.push(BorrowedMatch { builtin, signature })?;
            }
        }
    }
    let mut candidates = candidates.finish();
    if candidates.values.is_empty() {
        return Err(function_resolution_error(
            "42883",
            name,
            argument_names,
            argument_types,
            "does not exist",
        ));
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
    let matched = candidates
        .values
        .pop()
        .ok_or_else(|| SQLError::Internal("resolved built-in candidate disappeared".into()))?;
    Ok(matched)
}

pub(in crate::type_resolution) struct BorrowedMatch<'a> {
    pub(in crate::type_resolution) builtin: &'a BuiltinFunctionOverload,
    signature: Produced<MatchedFunctionSignature>,
}

impl RankedFunctionMatch for BorrowedMatch<'_> {
    fn argument_types(&self) -> &[String] {
        &self.signature.argument_types
    }
    fn raw_exact_matches(&self) -> usize {
        self.signature.raw_exact_matches
    }
    fn exact_matches(&self) -> usize {
        self.signature.exact_matches
    }
    fn preferred_matches(&self) -> usize {
        self.signature.preferred_matches
    }
}

fn resolved(
    matched: BorrowedMatch<'_>,
    argument_types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<ResolvedFunctionOverload>, SQLError> {
    let name = control.copy_text(&matched.builtin.name)?;
    let return_type = matched.builtin.return_type.clone_with_control(control)?;
    let mut types = ProductionVec::new(*control);
    types.reserve(matched.builtin.argument_types.len())?;
    for ty in &matched.builtin.argument_types {
        types.push_produced(ty.sql_name_with_control(control)?)?;
    }
    let types = types.finish()?;
    let (name, name_memory) = name.into_parts();
    let (return_type, return_memory) = return_type.into_parts();
    let (types, types_memory) = types.into_parts();
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
                exact_matches: matched.signature.exact_matches,
                known_arguments: argument_types.iter().flatten().count(),
                preferred_matches: matched.signature.preferred_matches,
                precedes_pg_catalog: false,
            },
            control.combine(name_memory, control.combine(return_memory, types_memory)),
        )
        .map_err(Into::into)
}

#[cfg(test)]
mod tests;
