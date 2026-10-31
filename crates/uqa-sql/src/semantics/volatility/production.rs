//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-aware mutability decisions borrow metadata and admit only normalization scratch.

use super::{FunctionBinding, FunctionVolatility, SQLError, ScalarExpr, VolatilityCatalog};
use crate::semantics::functions::lowercase_function_name_with_control;
use uqa_core::memory::ProductionControl;

pub fn builtin_function_volatility_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_count: usize,
    control: &ProductionControl<'_>,
) -> Result<FunctionVolatility, SQLError> {
    let name = lowercase_function_name_with_control(name, control)?;
    Ok(super::builtin_function_volatility(
        &name,
        binding,
        argument_count,
    ))
}

pub fn function_volatility_with_binding_and_control(
    catalog: &dyn VolatilityCatalog,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_count: usize,
    control: &ProductionControl<'_>,
) -> Result<FunctionVolatility, SQLError> {
    let builtin = builtin_function_volatility_with_control(name, binding, argument_count, control)?;
    if super::is_intrinsic_scalar_syntax(binding) || builtin == FunctionVolatility::Volatile {
        return Ok(builtin);
    }
    let identity = lowercase_function_name_with_control(name, control)?;
    if let Some(volatility) = catalog.host_function_volatility(&identity) {
        return Ok(volatility);
    }
    if let Some(volatility) =
        catalog.routine_volatility_with_control(&identity, binding, control)?
    {
        return Ok(volatility);
    }
    control.check()?;
    Ok(builtin)
}

pub(super) fn strongest(volatilities: &[FunctionVolatility]) -> FunctionVolatility {
    if volatilities.contains(&FunctionVolatility::Volatile) {
        FunctionVolatility::Volatile
    } else if volatilities.contains(&FunctionVolatility::Stable) {
        FunctionVolatility::Stable
    } else {
        FunctionVolatility::Immutable
    }
}

pub fn scalar_calls_are_volatile_with_control(
    catalog: &dyn VolatilityCatalog,
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    expression_contains_volatile_with_control(catalog, expression, false, control)
}

pub fn expr_contains_volatile_function_with_control(
    catalog: &dyn VolatilityCatalog,
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    expression_contains_volatile_with_control(catalog, expression, true, control)
}

pub(super) fn expression_contains_volatile_with_control(
    catalog: &dyn VolatilityCatalog,
    expression: &ScalarExpr,
    conservative_subqueries: bool,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let mut result = Ok(false);
    expression.visit(&mut |part| {
        if !matches!(result, Ok(false)) {
            return;
        }
        result = (|| {
            control.check()?;
            Ok(match part {
                ScalarExpr::Func {
                    name,
                    binding,
                    args,
                    ..
                } => {
                    function_volatility_with_binding_and_control(
                        catalog,
                        name,
                        binding.as_ref(),
                        args.len(),
                        control,
                    )? == FunctionVolatility::Volatile
                }
                ScalarExpr::WindowCall { name, args, .. } => {
                    function_volatility_with_binding_and_control(
                        catalog,
                        name,
                        None,
                        args.len(),
                        control,
                    )? == FunctionVolatility::Volatile
                }
                ScalarExpr::ScalarSubquery(_)
                | ScalarExpr::Exists { .. }
                | ScalarExpr::InSubquery { .. } => conservative_subqueries,
                _ => false,
            })
        })();
    });
    result
}

mod queries;
#[cfg(test)]
mod tests;
pub use queries::{
    query_contains_volatile_function_with_control,
    scalar_in_scope_contains_volatile_function_with_control,
    select_contains_volatile_function_with_control,
    unified_plan_contains_volatile_function_with_control,
};
