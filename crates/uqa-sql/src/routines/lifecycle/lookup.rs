//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lookup exact stored routine identities and visible or unshadowed overload candidates.

use super::RoutineRegistry;
mod production;
mod visibility;
use crate::{ast::FunctionBinding, routines::SQLUserFunction};
pub use production::lookup_sql_routine_candidates_by_keys_with_control;
use std::sync::Arc;
pub use visibility::{
    bound_routine_volatility_with_control, routine_volatility_by_keys_with_control,
    visible_function_returns_set_by_keys_with_control,
    visible_function_setness_by_keys_with_control,
};

pub fn lookup_sql_functions_by_keys(
    registry: &RoutineRegistry,
    keys: impl IntoIterator<Item = String>,
) -> Option<Vec<Arc<SQLUserFunction>>> {
    let mut visible = Vec::new();
    visibility::visit_visible(
        registry,
        keys,
        &uqa_core::memory::ProductionControl::uncontrolled(),
        |function| {
            visible.push(Arc::clone(function));
            Ok(())
        },
    )
    .expect("ordinary visible routine observation");
    (!visible.is_empty()).then_some(visible)
}

pub fn lookup_bound_sql_functions_by_binding(
    registry: &RoutineRegistry,
    binding: &FunctionBinding,
) -> Option<Vec<Arc<SQLUserFunction>>> {
    let Some(object_id) = binding.object_id else {
        return lookup_sql_functions_by_keys(registry, std::iter::once(binding.name.clone()));
    };
    let matches = registry
        .values()
        .flat_map(|overloads| overloads.iter())
        .filter(|function| function.def.object_id == Some(object_id))
        .cloned()
        .collect::<Vec<_>>();
    (!matches.is_empty()).then_some(matches)
}

/// Named notation needs candidates before later schemas are hidden by equal declared signatures.
pub fn lookup_sql_routine_candidates_by_keys(
    registry: &RoutineRegistry,
    keys: impl IntoIterator<Item = String>,
) -> Option<Vec<Arc<SQLUserFunction>>> {
    let candidates = keys
        .into_iter()
        .filter_map(|key| registry.get(&key))
        .flat_map(|overloads| overloads.iter().cloned())
        .collect::<Vec<_>>();
    (!candidates.is_empty()).then_some(candidates)
}

pub fn lookup_bound_sql_routine_candidates_by_binding(
    registry: &RoutineRegistry,
    binding: &FunctionBinding,
) -> Option<Vec<Arc<SQLUserFunction>>> {
    if binding.object_id.is_some() {
        lookup_bound_sql_functions_by_binding(registry, binding)
    } else {
        lookup_sql_routine_candidates_by_keys(registry, std::iter::once(binding.name.clone()))
    }
}

/// Match the first retained scalar identity without copying the registry's candidate vector or declared signature.
pub fn select_bound_function_with_control<'a>(
    candidates: impl Iterator<Item = &'a Arc<SQLUserFunction>>,
    binding: &FunctionBinding,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<Arc<SQLUserFunction>>, crate::SQLError> {
    control.check()?;
    if binding.builtin || binding.resolution_error.is_some() {
        return Ok(None);
    }
    let Some(_) = binding.object_id else {
        return Ok(None);
    };
    select_bound_routine_with_control(
        candidates,
        binding,
        crate::routines::resolution::RoutineCallKind::Function,
        control,
    )
}

/// Observe the selected function or procedure without allocating a candidate snapshot.
pub fn select_bound_routine_with_control<'a>(
    candidates: impl Iterator<Item = &'a Arc<SQLUserFunction>>,
    binding: &FunctionBinding,
    kind: crate::routines::resolution::RoutineCallKind,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<Arc<SQLUserFunction>>, crate::SQLError> {
    control.check()?;
    for function in candidates {
        control.check()?;
        let identity_matches = binding.object_id.map_or_else(
            || function.def.name == binding.name,
            |identity| function.def.object_id == Some(identity),
        );
        if function.def.is_procedure
            == (kind == crate::routines::resolution::RoutineCallKind::Procedure)
            && identity_matches
            && crate::routines::routine_signature_matches_with_control(
                &function.def,
                &binding.argument_types,
                control,
            )?
        {
            return Ok(Some(Arc::clone(function)));
        }
    }
    Ok(None)
}
