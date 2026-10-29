//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation retains the selected binding and argument permutation independently.

use super::{registry, resolve_overload_with_control};
use crate::type_resolution::{
    overload_resolution::match_signature_with_control, ResolvedFunctionOverload,
};
use crate::{ast::FunctionBinding, ColumnType, FunctionTypeResolver, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(crate) struct ControlledFixedBuiltinCall {
    pub selected: Produced<ResolvedFunctionOverload>,
    pub builtin_argument_positions: Option<Produced<Vec<usize>>>,
    pub builtin_non_immutable: bool,
}

pub(crate) fn resolve_fixed_builtin_call_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    names: &[Option<String>],
    types: &[Option<ColumnType>],
    variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<ControlledFixedBuiltinCall>, SQLError> {
    control.check()?;
    if binding.is_none()
        && resolver
            .map(|resolver| resolver.has_untyped_function_with_control(name, control))
            .transpose()?
            .unwrap_or(false)
    {
        return Ok(None);
    }
    if super::super::range::function_type(name, binding, types).is_some()
        || registry::lookup(name).is_none()
    {
        return Ok(None);
    }
    let selected =
        resolve_overload_with_control(name, binding, names, types, variadic, resolver, control)?;
    let (positions, non_immutable) = if selected.binding.builtin {
        let lost = || {
            SQLError::Internal(format!(
                "resolved fixed built-in `{}` lost its catalog signature",
                selected.binding.name
            ))
        };
        let signature = registry::bound_signature(&selected.binding, control)?.ok_or_else(lost)?;
        let matched =
            match_signature_with_control(signature, names, types, control)?.ok_or_else(lost)?;
        let mut positions = ProductionVec::new(*control);
        positions.extend_from_slice(&matched.argument_positions)?;
        (
            Some(positions.finish()?),
            super::builtin_binding_is_non_immutable(&selected.binding),
        )
    } else {
        (None, false)
    };
    Ok(Some(ControlledFixedBuiltinCall {
        selected,
        builtin_argument_positions: positions,
        builtin_non_immutable: non_immutable,
    }))
}

pub(in crate::type_resolution) fn selected_argument_targets_with_control(
    name: &str,
    types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Option<ColumnType>>>>, SQLError> {
    use crate::type_resolution::functions::targets::{copy_optional, semantic_optional};
    let mut names = ProductionVec::new(*control);
    names.reserve(types.len())?;
    for _ in 0..types.len() {
        names.push_produced(control.finish(None, control.empty_reservation())?)?;
    }
    let names = names.finish()?;
    let Some(selected) = semantic_optional(
        resolve_overload_with_control(name, None, &names, types, false, None, control).map(Some),
    )?
    else {
        return Ok(None);
    };
    let mut output = ProductionVec::new(*control);
    output.reserve(types.len())?;
    for (name, actual) in selected.binding.argument_types.iter().zip(types) {
        let parsed =
            semantic_optional(ColumnType::from_sql_name_with_control(name, control).map(Some))?;
        let parsed = if parsed.as_deref() == Some(&ColumnType::AnyArray) {
            copy_optional(actual.as_ref(), control)?
        } else {
            let (ty, memory) = parsed.map_or((None, control.empty_reservation()), |ty| {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            });
            control.finish(ty, memory)?
        };
        output.push_produced(parsed)?;
    }
    if output.len() != types.len() {
        return Ok(None);
    }
    Ok(Some(output.finish()?))
}
