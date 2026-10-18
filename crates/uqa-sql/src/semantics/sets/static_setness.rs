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
    // A call bound to a SQL routine returns a set exactly when that routine does, whatever built-in shares its name.
    if let Some(binding) = binding.filter(|binding| !binding.builtin) {
        return catalog
            .lookup_bound_sql_functions_by_binding(binding)
            .is_some_and(|overloads| {
                overloads.iter().any(|function| {
                    !function.def.is_procedure
                        && crate::routines::routine_signature_types(&function.def)
                            == binding.argument_types
                        && function.def.returns_set()
                })
            });
    }
    let identity = name.to_ascii_lowercase();
    let builtin = crate::semantics::builtin_function_dispatch_name(&identity);
    if builtin_returns_set(&builtin) || catalog.has_registered_table_function(&identity) {
        return true;
    }
    if binding.is_some() {
        return false;
    }
    catalog
        .lookup_visible_sql_functions_for_analysis(name)
        .ok()
        .flatten()
        .is_some_and(|overloads| {
            overloads
                .iter()
                .any(|function| !function.def.is_procedure && function.def.returns_set())
        })
}

/// Whether a call of a built-in routine returns a set: a call bound to a built-in, or an unbound call that no SQL routine resolved. Scalar evaluation reports such a call with `PostgreSQL`'s executor diagnostic instead of evaluating it.
pub fn builtin_call_returns_set(name: &str, binding: Option<&FunctionBinding>) -> bool {
    if binding.is_some_and(|binding| {
        !binding.builtin || binding.dispatch.is_some() || binding.is_polymorphic_builtin_syntax()
    }) {
        return false;
    }
    let name = binding
        .map_or(name, |binding| binding.name.as_str())
        .to_ascii_lowercase();
    builtin_returns_set(name.strip_prefix("pg_catalog.").unwrap_or(&name))
}

/// Whether `expression` contains a call that returns a set, outside its subqueries, classified from the catalog alone: a bound call by its routine, an unbound call by every visible routine of its name.
pub fn expression_may_return_set_statically(
    catalog: &dyn crate::routines::RoutineResolution,
    expression: &crate::ScalarExpr,
) -> bool {
    let mut returns_set = false;
    expression.visit(&mut |node| {
        if let crate::ScalarExpr::Func { name, binding, .. } = node {
            if !returns_set {
                returns_set = function_may_return_set_statically(catalog, name, binding.as_ref());
            }
        }
    });
    returns_set
}
