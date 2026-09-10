//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-only set-returning routine classification before physical traversal.
use super::validation::builtin_returns_set;
use crate::ast::FunctionBinding;

pub fn function_may_return_set_statically(
    catalog: &dyn crate::routines::RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
) -> bool {
    if binding.and_then(|binding| binding.dispatch).is_some()
        || binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax)
    {
        return false;
    }
    let identity = name.to_ascii_lowercase();
    let builtin = crate::semantics::builtin_function_dispatch_name(&identity);
    if builtin_returns_set(&builtin) || catalog.has_registered_table_function(&identity) {
        return true;
    }
    if binding.is_some_and(|binding| binding.builtin) {
        return false;
    }
    let overloads = match binding {
        Some(binding) => catalog.lookup_bound_sql_functions_by_binding(binding),
        None => catalog
            .lookup_visible_sql_functions_for_analysis(name)
            .ok()
            .flatten(),
    };
    let Some(overloads) = overloads else {
        return false;
    };
    if let Some(binding) = binding {
        return overloads.iter().any(|function| {
            !function.def.is_procedure
                && crate::routines::routine_signature_types(&function.def) == binding.argument_types
                && function.def.returns_set()
        });
    }
    overloads
        .iter()
        .any(|function| !function.def.is_procedure && function.def.returns_set())
}
