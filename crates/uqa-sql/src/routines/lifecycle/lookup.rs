//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lookup exact stored routine identities and visible or unshadowed overload candidates.

use super::RoutineRegistry;
use crate::{
    ast::FunctionBinding,
    routines::{routine_signature_types, SQLUserFunction},
};
use std::{collections::BTreeSet, sync::Arc};

pub fn lookup_sql_functions_by_keys(
    registry: &RoutineRegistry,
    keys: impl IntoIterator<Item = String>,
) -> Option<Vec<Arc<SQLUserFunction>>> {
    let mut visible = Vec::new();
    let mut seen = BTreeSet::new();
    for key in keys {
        let Some(overloads) = registry.get(&key) else {
            continue;
        };
        for function in overloads {
            let identity = (
                routine_signature_types(&function.def),
                function.def.is_procedure,
            );
            if seen.insert(identity) {
                visible.push(function.clone());
            }
        }
    }
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
