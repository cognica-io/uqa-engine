//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-only set-returning routine classification before physical traversal.
use super::validation::builtin_returns_set;
use crate::ast::FunctionBinding;
use crate::SQLError;
use uqa_core::memory::ProductionControl;

pub fn function_may_return_set_statically(
    catalog: &dyn crate::routines::RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
) -> bool {
    function_may_return_set_statically_with_control(
        catalog,
        name,
        binding,
        &ProductionControl::uncontrolled(),
    )
    .expect("ordinary static setness uses no limited resources")
}

pub fn function_may_return_set_statically_with_control(
    catalog: &dyn crate::routines::RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if binding.and_then(|binding| binding.dispatch).is_some()
        || binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax)
    {
        return Ok(false);
    }
    // A call bound to a SQL routine returns a set exactly when that routine does, whatever built-in shares its name.
    if let Some(binding) = binding.filter(|binding| !binding.builtin) {
        return Ok(crate::routines::selected_bound_function_with_control(
            catalog, binding, control,
        )?
        .is_some_and(|function| function.def.returns_set()));
    }
    let identity = crate::semantics::lowercase_function_name_with_control(name, control)?;
    let builtin =
        crate::semantics::builtin_function_dispatch_name_with_control(&identity, control)?;
    if builtin_returns_set(&builtin)
        || catalog.has_registered_table_function_with_control(&identity, control)?
    {
        return Ok(true);
    }
    if binding.is_some() {
        return Ok(false);
    }
    match catalog.visible_function_returns_set_with_control(name, control) {
        Ok(returns_set) => Ok(returns_set),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(false),
    }
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
    expression_may_return_set_statically_with_control(
        catalog,
        expression,
        &ProductionControl::uncontrolled(),
    )
    .expect("ordinary static setness uses no limited resources")
}

pub fn expression_may_return_set_statically_with_control(
    catalog: &dyn crate::routines::RoutineResolution,
    expression: &crate::ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut returns_set = false;
    expression.try_visit(&mut |node| {
        control.check()?;
        if let crate::ScalarExpr::Func { name, binding, .. } = node {
            if !returns_set {
                returns_set = function_may_return_set_statically_with_control(
                    catalog,
                    name,
                    binding.as_ref(),
                    control,
                )?;
            }
        }
        Ok::<_, SQLError>(true)
    })?;
    Ok(returns_set)
}
